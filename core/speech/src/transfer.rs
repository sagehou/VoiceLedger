use std::io::{self, Read};
use std::time::Duration;

use ureq::Agent;
use ureq::config::Config;
use ureq::http::{HeaderMap, StatusCode};
use voiceledger_core::models::{Artifact, Download};

pub fn fetcher(
    cancelled: &dyn Fn() -> bool,
) -> impl FnMut(&Artifact, &str, u64) -> io::Result<Download> + '_ {
    let probe: Agent = Config::builder()
        .https_only(true)
        .max_redirects(8)
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();
    let setup = Some(Duration::from_secs(30));
    let download: Agent = Config::builder()
        .https_only(true)
        .max_redirects(8)
        .timeout_resolve(setup)
        .timeout_connect(setup)
        .timeout_send_request(setup)
        .timeout_recv_response(setup)
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into();
    move |artifact, url, offset| {
        probe_source(&probe, url, offset, artifact.size_bytes)?;
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        fetch(&download, artifact, url, offset)
    }
}

fn probe_source(agent: &Agent, url: &str, offset: u64, size: u64) -> io::Result<()> {
    let response = agent
        .get(url)
        .header("Accept-Encoding", "identity")
        .header("Cache-Control", "no-store")
        .header("Range", format!("bytes={offset}-{offset}"))
        .call()
        .map_err(|_| transfer_error())?;
    let reachable = match response.status() {
        StatusCode::PARTIAL_CONTENT => matches_range(response.headers(), offset, offset, size),
        StatusCode::OK => content_length(response.headers()).is_none_or(|length| length == size),
        _ => false,
    };
    if !reachable {
        return Err(transfer_error());
    }
    response.into_body().into_reader().read_exact(&mut [0])
}

fn fetch(agent: &Agent, artifact: &Artifact, url: &str, offset: u64) -> io::Result<Download> {
    let mut request = agent.get(url).header("Accept-Encoding", "identity");
    if offset != 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    let response = request.call().map_err(|_| transfer_error())?;
    let start = match response.status() {
        StatusCode::OK
            if content_length(response.headers())
                .is_none_or(|size| size == artifact.size_bytes) =>
        {
            0
        }
        StatusCode::PARTIAL_CONTENT
            if offset != 0
                && matches_range(
                    response.headers(),
                    offset,
                    artifact.size_bytes - 1,
                    artifact.size_bytes,
                ) =>
        {
            offset
        }
        _ => return Err(transfer_error()),
    };
    Ok(Download {
        start,
        body: Box::new(response.into_body().into_reader()),
    })
}

fn matches_range(headers: &HeaderMap, start: u64, end: u64, size: u64) -> bool {
    headers
        .get("Content-Range")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("bytes {start}-{end}/{size}"))
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers.get("Content-Length")?.to_str().ok()?.parse().ok()
}

fn transfer_error() -> io::Error {
    io::Error::other("Model transfer failed.")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    use voiceledger_core::models::Catalog;

    use super::*;

    #[test]
    fn probes_the_requested_range_and_opens_a_resumed_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for (expected, range, body) in [
                ("range: bytes=1-1", "bytes 0-0/3", "a"),
                ("range: bytes=1-1", "bytes 1-1/3", "b"),
                ("range: bytes=1-", "bytes 1-2/3", "bc"),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut saw_range = false;
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    saw_range |= line.trim_end().eq_ignore_ascii_case(expected);
                    if line == "\r\n" {
                        break;
                    }
                }
                assert!(saw_range);
                write!(
                    socket,
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Range: {range}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let agent: Agent = Config::builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .into();
        assert!(probe_source(&agent, &url, 1, 3).is_err());
        probe_source(&agent, &url, 1, 3).unwrap();
        let mut artifact = Catalog::bundled().unwrap().models()[0].artifacts[0].clone();
        artifact.size_bytes = 3;
        let mut download = fetch(&agent, &artifact, &url, 1).unwrap();
        assert_eq!(download.start, 1);
        let mut body = Vec::new();
        download.body.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"bc");
        server.join().unwrap();
    }
}
