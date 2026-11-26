use std::io;
use std::time::Duration;

use ureq::Agent;
use ureq::config::Config;
use ureq::http::StatusCode;
use voiceledger_core::models::{Artifact, Download};

pub fn agent() -> Agent {
    let setup = Some(Duration::from_secs(30));
    Config::builder()
        .https_only(true)
        .max_redirects(8)
        .timeout_resolve(setup)
        .timeout_connect(setup)
        .timeout_send_request(setup)
        .timeout_recv_response(setup)
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
}

pub fn fetch(agent: &Agent, artifact: &Artifact, offset: u64) -> io::Result<Download> {
    let mut request = agent
        .get(artifact.url.as_str())
        .header("Accept-Encoding", "identity");
    if offset != 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    let failed = || io::Error::other("Model transfer failed.");
    let response = request.call().map_err(|_| failed())?;
    let start = match response.status() {
        StatusCode::OK => 0,
        StatusCode::PARTIAL_CONTENT if offset != 0 => {
            let expect = format!(
                "bytes {offset}-{}/{}",
                artifact.size_bytes - 1,
                artifact.size_bytes
            );
            if response
                .headers()
                .get("Content-Range")
                .and_then(|value| value.to_str().ok())
                != Some(expect.as_str())
            {
                return Err(failed());
            }
            offset
        }
        _ => return Err(failed()),
    };
    Ok(Download {
        start,
        body: Box::new(response.into_body().into_reader()),
    })
}
