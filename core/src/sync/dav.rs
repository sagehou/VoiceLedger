use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use roxmltree::Document;
use ureq::config::Config;
use ureq::http::{Request, Response, Uri};
use ureq::{Agent, Body};
use zeroize::Zeroizing;

use super::{ObjectKey, Page, PutOutcome, SyncBackend, SyncError};
use crate::vault::{MAX_PLAINTEXT_BYTES, SealedObject};

const LIMIT_XML: u64 = 4 * 1024 * 1024;
const DAV: &str = "DAV:";
const PROPERTIES: &[u8] = br#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;

pub struct WebDav {
    agent: Agent,
    origin: String,
    root: String,
    auth: Zeroizing<String>,
}

impl WebDav {
    pub fn new(endpoint: &str, name: &str, credential: &str) -> Result<Self, SyncError> {
        Self::connect(endpoint, name, credential, true)
    }

    fn connect(endpoint: &str, name: &str, credential: &str, tls: bool) -> Result<Self, SyncError> {
        let uri: Uri = endpoint.parse().map_err(|_| SyncError::Configuration)?;
        let scheme = if tls { "https" } else { "http" };
        let authority = uri.authority().ok_or(SyncError::Configuration)?;
        if uri.scheme_str() != Some(scheme)
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || name.contains(':')
            || name.is_empty()
            || name.len() + credential.len() > 4096
        {
            return Err(SyncError::Configuration);
        }
        let pair = Zeroizing::new(format!("{name}:{credential}"));
        let mut auth = Zeroizing::new(String::from("Basic "));
        STANDARD.encode_string(pair.as_bytes(), &mut auth);
        let mut config = Config::builder()
            .https_only(tls)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)));
        if !tls {
            config = config.proxy(None);
        }
        Ok(Self {
            agent: config.build().into(),
            origin: format!("{scheme}://{authority}"),
            root: uri.path().trim_end_matches('/').to_owned(),
            auth,
        })
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response<Body>, SyncError> {
        let url = format!("{}{}{}", self.origin, self.root, path);
        let mut request = Request::builder()
            .method(method)
            .uri(url)
            .header("Authorization", self.auth.as_str())
            .header("Accept-Encoding", "identity");
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let request = request.body(body).map_err(|_| SyncError::Configuration)?;
        let response = self.agent.run(request).map_err(|_| SyncError::Transport)?;
        match response.status().as_u16() {
            300..=399 => Err(SyncError::Redirect),
            401 | 403 => Err(SyncError::AuthenticationRequired),
            404 => Err(SyncError::NotFound),
            408 | 425 | 429 | 500..=599 if response.status().as_u16() != 507 => {
                Err(SyncError::Retry)
            }
            507 => Err(SyncError::Quota),
            _ => Ok(response),
        }
    }
}

impl SyncBackend for WebDav {
    fn put_immutable(&self, key: &ObjectKey, body: &SealedObject) -> Result<PutOutcome, SyncError> {
        let path = format!("/{}", key.value());
        for (index, _) in path.match_indices('/').skip(1) {
            let response = self.send("MKCOL", &path[..index], &[], &[])?;
            if !matches!(response.status().as_u16(), 201 | 405) {
                return Err(SyncError::ProviderUnsupported);
            }
        }
        let response = self.send(
            "PUT",
            &path,
            &[
                ("If-None-Match", "*"),
                ("Content-Type", "application/octet-stream"),
            ],
            body.envelope(),
        )?;
        match response.status().as_u16() {
            201 | 204 => Ok(PutOutcome::Created),
            412 => Ok(PutOutcome::Exists),
            _ => Err(SyncError::ProviderUnsupported),
        }
    }

    fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, SyncError> {
        let mut response = self.send("GET", &format!("/{}", key.value()), &[], &[])?;
        if response.status().as_u16() != 200 {
            return Err(SyncError::ProviderUnsupported);
        }
        response
            .body_mut()
            .with_config()
            .limit((MAX_PLAINTEXT_BYTES + 64) as u64)
            .read_to_vec()
            .map_err(|_| SyncError::Integrity)
    }

    fn list(&self, prefix: &ObjectKey, continuation: Option<&str>) -> Result<Page, SyncError> {
        if continuation.is_some() {
            return Err(SyncError::ProviderUnsupported);
        }
        let path = format!("/{}/", prefix.value());
        let mut response = match self.send(
            "PROPFIND",
            &path,
            &[("Depth", "1"), ("Content-Type", "application/xml")],
            PROPERTIES,
        ) {
            Ok(value) => value,
            Err(SyncError::NotFound) => {
                return Ok(Page {
                    keys: Vec::new(),
                    continuation: None,
                });
            }
            Err(error) => return Err(error),
        };
        if response.status().as_u16() != 207 {
            return Err(SyncError::ProviderUnsupported);
        }
        let xml = response
            .body_mut()
            .with_config()
            .limit(LIMIT_XML)
            .read_to_string()
            .map_err(|_| SyncError::Integrity)?;
        let doc = Document::parse(&xml).map_err(|_| SyncError::Integrity)?;
        let multistatus = doc.root_element();
        if !multistatus.has_tag_name((DAV, "multistatus")) {
            return Err(SyncError::Integrity);
        }
        let expected = format!("{}{}", self.root, path);
        let mut keys = Vec::new();
        for entry in multistatus
            .children()
            .filter(|node| node.has_tag_name((DAV, "response")))
        {
            let href = entry
                .children()
                .find(|node| node.has_tag_name((DAV, "href")))
                .and_then(|node| node.text())
                .ok_or(SyncError::Integrity)?;
            let relative = href.strip_prefix(self.origin.as_str()).unwrap_or(href);
            if let Some(name) = relative.strip_prefix(&expected)
                && !name.is_empty()
                && !name.contains('/')
            {
                keys.push(
                    ObjectKey::new(format!("{}/{name}", prefix.value()))
                        .map_err(|_| SyncError::Integrity)?,
                );
            }
        }
        keys.sort_unstable();
        keys.dedup();
        Ok(Page {
            keys,
            continuation: None,
        })
    }

    fn delete(&self, key: &ObjectKey) -> Result<(), SyncError> {
        match self.send("DELETE", &format!("/{}", key.value()), &[], &[]) {
            Err(SyncError::NotFound) => Ok(()),
            Ok(response) if matches!(response.status().as_u16(), 200 | 204) => Ok(()),
            Ok(_) => Err(SyncError::ProviderUnsupported),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::SyncEvent;
    use crate::sync::{PendingEvent, device_prefix, read_event};
    use crate::vault::{SyncScopes, Vault};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn webdav_retries_the_same_ciphertext_and_reads_the_event() {
        let journal: serde_json::Value =
            serde_json::from_str(include_str!("../../../contracts/fixtures/domain-v1.json"))
                .unwrap();
        let event: SyncEvent = serde_json::from_value(journal["sync_events"][0].clone()).unwrap();
        let vault = Vault::create().unwrap();
        let scopes = SyncScopes {
            enabled: true,
            raw_audio: true,
            ..SyncScopes::default()
        };
        let upload = PendingEvent::seal(&vault, scopes, &event).unwrap();
        let id = upload.key().value().to_owned();
        let prefix = device_prefix(&vault, &event.device_id).unwrap();
        let target = format!("/dav/{id}");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/dav", listener.local_addr().unwrap());
        let service = thread::spawn(move || {
            let collection_count = id.matches('/').count();
            let requests = collection_count * 2 + 10;
            let mut sealed = Vec::new();
            for _ in 0..requests {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&socket);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut split = line.split_whitespace();
                let method = split.next().unwrap();
                let path = split.next().unwrap();
                let mut length = 0;
                let mut conditional = false;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                    if name.eq_ignore_ascii_case("if-none-match") {
                        conditional = value.trim() == "*";
                    }
                }
                let mut request = vec![0; length];
                reader.read_exact(&mut request).unwrap();
                let (status, headers, body) = match method {
                    "MKCOL" => ("405 Method Not Allowed", "", Vec::new()),
                    "PUT" if sealed.is_empty() => {
                        assert!(conditional && path == target && request.starts_with(b"VLO1"));
                        sealed = request;
                        ("201 Created", "", Vec::new())
                    }
                    "PUT" => {
                        assert!(conditional && request == sealed);
                        ("412 Precondition Failed", "", Vec::new())
                    }
                    "PROPFIND" => {
                        let xml = format!(
                            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>{target}</d:href></d:response></d:multistatus>"#
                        );
                        ("207 Multi-Status", "", xml.into_bytes())
                    }
                    "GET" if path.ends_with("/redirect") => (
                        "301 Moved Permanently",
                        "Location: /dav/unexpected\r\n",
                        Vec::new(),
                    ),
                    "GET" if path.ends_with("/busy") => ("503 Service Unavailable", "", Vec::new()),
                    "GET" if path.ends_with("/quota") => {
                        ("507 Insufficient Storage", "", Vec::new())
                    }
                    "GET" if path.ends_with("/private") => ("401 Unauthorized", "", Vec::new()),
                    "GET" => {
                        assert_eq!(path, target);
                        ("200 OK", "", sealed.clone())
                    }
                    "DELETE" => ("204 No Content", "", Vec::new()),
                    _ => panic!("Unexpected HTTP method"),
                };
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(&body).unwrap();
            }
        });

        let backend = WebDav::connect(&endpoint, "synthetic", "fixture", false).unwrap();
        upload.publish(&backend).unwrap();
        upload.publish(&backend).unwrap();
        assert_eq!(
            backend.list(&prefix, None).unwrap().keys,
            vec![upload.key().clone()]
        );
        assert_eq!(
            read_event(&vault, &backend, upload.key()).unwrap().id,
            event.id
        );
        for (key, expected) in [
            ("redirect", SyncError::Redirect),
            ("busy", SyncError::Retry),
            ("quota", SyncError::Quota),
            ("private", SyncError::AuthenticationRequired),
        ] {
            assert_eq!(
                backend
                    .get(&ObjectKey::new(key.to_owned()).unwrap())
                    .unwrap_err(),
                expected
            );
        }
        backend.delete(upload.key()).unwrap();
        service.join().unwrap();
    }

    #[test]
    fn rejects_cleartext_or_credentialed_endpoints_and_path_traversal() {
        assert!(WebDav::new("http://example.test", "synthetic", "fixture").is_err());
        assert!(WebDav::new("https://user@example.test", "synthetic", "fixture").is_err());
        assert!(ObjectKey::new("dev/../other".to_owned()).is_err());
        assert!(ObjectKey::new("dev/%2e%2e".to_owned()).is_err());
    }
}
