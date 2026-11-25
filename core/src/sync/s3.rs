use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
    UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4;
use roxmltree::Document;
use sha2::{Digest, Sha256};
use ureq::config::Config;
use ureq::http::{Request, Response, Uri};
use ureq::{Agent, Body};

use super::{ObjectKey, Page, PutOutcome, SyncBackend, SyncError, hex, storage_status};
use crate::vault::{MAX_PLAINTEXT_BYTES, SealedObject};

const LIMIT_XML: u64 = 4 * 1024 * 1024;

pub struct S3 {
    agent: Agent,
    bucket_url: String,
    region: String,
    credentials: Credentials,
}

impl S3 {
    /// Connect to a path-style S3-compatible HTTPS endpoint.
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
    ) -> Result<Self, SyncError> {
        Self::connect(
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            session_token,
            true,
        )
    }

    fn connect(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
        tls: bool,
    ) -> Result<Self, SyncError> {
        let uri: Uri = endpoint.parse().map_err(|_| SyncError::Configuration)?;
        let scheme = if tls { "https" } else { "http" };
        let authority = uri.authority().ok_or(SyncError::Configuration)?;
        let name_byte =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        let path = uri.path().trim_matches('/');
        if uri.scheme_str() != Some(scheme)
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || (!path.is_empty() && ObjectKey::new(path.to_owned()).is_err())
            || !(3..=63).contains(&bucket.len())
            || !bucket.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(name_byte)
            })
            || region.is_empty()
            || region.len() > 64
            || !region.bytes().all(name_byte)
            || !credential_value(access_key, 128)
            || access_key.bytes().any(|byte| !byte.is_ascii_alphanumeric())
            || !credential_value(secret_key, 256)
            || session_token.is_some_and(|token| !credential_value(token, 16 * 1024))
        {
            return Err(SyncError::Configuration);
        }
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
            bucket_url: format!(
                "{scheme}://{authority}{}/{bucket}",
                uri.path().trim_end_matches('/')
            ),
            region: region.to_owned(),
            credentials: Credentials::new(
                access_key,
                secret_key,
                session_token.map(str::to_owned),
                None,
                "VoiceLedger",
            ),
        })
    }

    fn send(
        &self,
        method: &str,
        suffix: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response<Body>, SyncError> {
        let url = format!("{}{suffix}", self.bucket_url);
        let identity = self.credentials.clone().into();
        let mut settings = SigningSettings::default();
        settings.percent_encoding_mode = PercentEncodingMode::Single;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        let parameters = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|_| SyncError::Configuration)?
            .into();
        let input = SignableRequest::new(
            method,
            url.as_str(),
            headers.iter().copied(),
            SignableBody::Precomputed(hex(&Sha256::digest(body))),
        )
        .map_err(|_| SyncError::Configuration)?;
        let (signature, _) = sign(input, &parameters)
            .map_err(|_| SyncError::Configuration)?
            .into_parts();
        let mut request = Request::builder()
            .method(method)
            .uri(url.as_str())
            .header("Accept-Encoding", "identity");
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let mut request = request.body(body).map_err(|_| SyncError::Configuration)?;
        signature.apply_to_request_http1x(&mut request);
        let response = self.agent.run(request).map_err(|_| SyncError::Transport)?;
        let status = response.status().as_u16();
        if status == 409 && method == "PUT" {
            return Err(SyncError::Retry);
        }
        if status == 400 {
            let mut response = response;
            return Err(s3_client_error(&mut response)?);
        }
        storage_status(status)?;
        Ok(response)
    }
}

impl SyncBackend for S3 {
    fn put_immutable(&self, key: &ObjectKey, body: &SealedObject) -> Result<PutOutcome, SyncError> {
        let response = self.send(
            "PUT",
            &format!("/{}", key.value()),
            &[
                ("If-None-Match", "*"),
                ("Content-Type", "application/octet-stream"),
            ],
            body.envelope(),
        )?;
        match response.status().as_u16() {
            200 | 201 | 204 => Ok(PutOutcome::Created),
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
        let parent = format!("{}/", prefix.value());
        let mut query = format!("?list-type=2&max-keys=1000&prefix={}", escape(&parent));
        if let Some(token) = continuation {
            if !valid_token(token) {
                return Err(SyncError::Configuration);
            }
            query.push_str("&continuation-token=");
            query.push_str(&escape(token));
        }
        let mut response = self.send("GET", &query, &[], &[])?;
        if response.status().as_u16() != 200 {
            return Err(SyncError::ProviderUnsupported);
        }
        let xml = response
            .body_mut()
            .with_config()
            .limit(LIMIT_XML)
            .read_to_string()
            .map_err(|_| SyncError::Integrity)?;
        parse_list(&xml, &parent, continuation)
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

fn parse_list(xml: &str, parent: &str, sent_token: Option<&str>) -> Result<Page, SyncError> {
    let document = Document::parse(xml).map_err(|_| SyncError::Integrity)?;
    let root = document.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err(SyncError::Integrity);
    }
    let field = |name: &str| {
        root.children()
            .find(|node| node.tag_name().name() == name)
            .and_then(|node| node.text())
    };
    let continuation = match (field("IsTruncated"), field("NextContinuationToken")) {
        (Some("false"), None) => None,
        (Some("true"), Some(token)) if valid_token(token) && sent_token != Some(token) => {
            Some(token.to_owned())
        }
        _ => return Err(SyncError::Integrity),
    };
    let mut keys = Vec::new();
    for object in root
        .children()
        .filter(|node| node.tag_name().name() == "Contents")
    {
        let name = object
            .children()
            .find(|node| node.tag_name().name() == "Key")
            .and_then(|node| node.text())
            .ok_or(SyncError::Integrity)?;
        if !name.starts_with(parent) {
            return Err(SyncError::Integrity);
        }
        if name != parent {
            keys.push(ObjectKey::new(name.to_owned()).map_err(|_| SyncError::Integrity)?);
        }
        if keys.len() > 1000 {
            return Err(SyncError::Integrity);
        }
    }
    Ok(Page { keys, continuation })
}

fn s3_client_error(response: &mut Response<Body>) -> Result<SyncError, SyncError> {
    let xml = response
        .body_mut()
        .with_config()
        .limit(32 * 1024)
        .read_to_string()
        .map_err(|_| SyncError::ProviderUnsupported)?;
    let document = Document::parse(&xml).map_err(|_| SyncError::ProviderUnsupported)?;
    let code = document
        .root_element()
        .children()
        .find(|node| node.tag_name().name() == "Code")
        .and_then(|node| node.text());
    Ok(match code {
        Some(
            "ExpiredToken"
            | "InvalidToken"
            | "InvalidAccessKeyId"
            | "SignatureDoesNotMatch"
            | "RequestTimeTooSkewed",
        ) => SyncError::AuthenticationRequired,
        Some("SlowDown" | "RequestTimeout") => SyncError::Retry,
        Some("QuotaExceeded" | "InsufficientStorage") => SyncError::Quota,
        _ => SyncError::ProviderUnsupported,
    })
}

fn credential_value(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control)
}

fn escape(value: &str) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("writing into a String succeeds");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::vault::Vault;

    #[test]
    fn s3_uses_conditional_put_and_follows_opaque_pagination() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let vault = Vault::create().unwrap();
        let sealed = vault.seal(b"synthetic provider check").unwrap();
        let bytes = sealed.envelope().to_vec();
        let service = thread::spawn(move || {
            let list = "?list-type=2&max-keys=1000&prefix=private%2F";
            let first_page = "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>true</IsTruncated><Contents><Key>private/one</Key></Contents><NextContinuationToken>A+/=</NextContinuationToken></ListBucketResult>";
            let last_page = "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>private/two</Key></Contents></ListBucketResult>";
            let encoded_token = concat!("A%", "2B%", "2F%", "3D");
            let continuation = format!("/test-bucket{list}&continuation-token={encoded_token}");
            let first_listing = format!("/test-bucket{list}");
            let requests: [(&str, &str, &str, &[u8]); 10] = [
                ("PUT", "/test-bucket/private/one", "200 OK", b""),
                (
                    "PUT",
                    "/test-bucket/private/one",
                    "412 Precondition Failed",
                    b"",
                ),
                ("GET", "/test-bucket/private/one", "200 OK", &bytes),
                ("GET", &first_listing, "200 OK", first_page.as_bytes()),
                ("GET", &continuation, "200 OK", last_page.as_bytes()),
                (
                    "GET",
                    "/test-bucket/private/expired",
                    "400 Bad Request",
                    b"<Error><Code>ExpiredToken</Code></Error>",
                ),
                (
                    "GET",
                    "/test-bucket/private/redirect",
                    "307 Temporary Redirect",
                    b"",
                ),
                (
                    "GET",
                    "/test-bucket/private/quota",
                    "507 Insufficient Storage",
                    b"",
                ),
                ("PUT", "/test-bucket/private/retry", "409 Conflict", b""),
                ("DELETE", "/test-bucket/private/one", "204 No Content", b""),
            ];
            for (method, target, status, body) in requests {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&socket);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), format!("{method} {target} HTTP/1.1"));
                let mut length = 0;
                let mut auth = false;
                let mut put_condition = false;
                let mut token = false;
                let mut hash = None;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.trim_end().split_once(':').unwrap();
                    let value = value.trim();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.parse().unwrap(),
                        "authorization" => {
                            auth = value.starts_with("AWS4-HMAC-SHA256 Credential=ExampleAccess/")
                                && value.contains("/us-east-1/s3/aws4_request")
                        }
                        "if-none-match" => put_condition = value == "*",
                        "x-amz-security-token" => token = value == "synthetic-session-token",
                        "x-amz-content-sha256" => hash = Some(value.to_owned()),
                        _ => {}
                    }
                }
                let mut input = vec![0; length];
                reader.read_exact(&mut input).unwrap();
                assert!(auth && token);
                assert_eq!(hash.unwrap(), hex(&Sha256::digest(&input)));
                if method == "PUT" {
                    assert!(put_condition);
                    assert_eq!(input, bytes);
                } else {
                    assert!(input.is_empty());
                }
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(body).unwrap();
            }
        });
        let backend = S3::connect(
            &endpoint,
            "test-bucket",
            "us-east-1",
            "ExampleAccess",
            "SyntheticSecret",
            Some("synthetic-session-token"),
            false,
        )
        .unwrap();
        let key = |name: &str| ObjectKey::new(format!("private/{name}")).unwrap();
        let prefix = ObjectKey::new("private".to_owned()).unwrap();
        let first = key("one");
        assert_eq!(
            backend.put_immutable(&first, &sealed),
            Ok(PutOutcome::Created)
        );
        assert_eq!(
            backend.put_immutable(&first, &sealed),
            Ok(PutOutcome::Exists)
        );
        assert_eq!(backend.get(&first).unwrap(), sealed.envelope());
        let listed = backend.list(&prefix, None).unwrap();
        assert_eq!(listed.keys, vec![first.clone()]);
        assert_eq!(listed.continuation.as_deref(), Some("A+/="));
        let last = backend
            .list(&prefix, listed.continuation.as_deref())
            .unwrap();
        assert_eq!(last.keys, vec![key("two")]);
        assert!(last.continuation.is_none());
        assert_eq!(
            backend.get(&key("expired")),
            Err(SyncError::AuthenticationRequired)
        );
        assert_eq!(backend.get(&key("redirect")), Err(SyncError::Redirect));
        assert_eq!(backend.get(&key("quota")), Err(SyncError::Quota));
        assert_eq!(
            backend.put_immutable(&key("retry"), &sealed),
            Err(SyncError::Retry)
        );
        assert_eq!(backend.delete(&first), Ok(()));
        service.join().unwrap();
    }
}
