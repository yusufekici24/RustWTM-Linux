//! HTTPS requests with ureq (rustls, verified against the system's trust
//! store so a corporate root CA works), under the same size and time
//! bounds as the Windows WinHTTP version. Proxies come from the usual
//! `https_proxy` / `HTTPS_PROXY` variables.

use std::time::Duration;

use sha2::Digest;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
struct Limit {
    bytes: usize,
    time: Duration,
}

#[allow(clippy::too_many_arguments)]
fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, &[u8])>,
    user_agent: &str,
    timeout_ms: i32,
    limit: Limit,
    follow_redirects: bool,
) -> Result<(u16, Vec<u8>)> {
    if !url.starts_with("https://")
        && !url.starts_with("http://127.0.0.1")
        && !url.starts_with("http://localhost")
    {
        return Err(Error::Message("only HTTPS URLs are allowed".into()));
    }
    let per_op = Duration::from_millis(timeout_ms.max(1000) as u64);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .http_status_as_error(false)
        .max_redirects(if follow_redirects { 5 } else { 0 })
        .max_redirects_will_error(false)
        .user_agent(user_agent)
        .timeout_global(Some(limit.time))
        .timeout_connect(Some(per_op.min(Duration::from_secs(30))))
        .timeout_recv_body(Some(per_op.max(Duration::from_secs(30))))
        .build()
        .into();
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let response = match body {
        Some((content_type, bytes)) => {
            let req = builder
                .header("Content-Type", content_type)
                .body(bytes.to_vec())
                .map_err(|e| Error::Message(e.to_string()))?;
            agent.run(req)
        }
        None => {
            let req = builder
                .body(())
                .map_err(|e| Error::Message(e.to_string()))?;
            agent.run(req)
        }
    };
    let mut response = response.map_err(|e| Error::Message(format!("{method} {url}: {e}")))?;
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit.bytes as u64)
        .read_to_vec()
        .map_err(|e| Error::Message(format!("reading {url}: {e}")))?;
    Ok((status, bytes))
}

pub fn get(url: &str, user_agent: &str) -> Result<(u16, Vec<u8>)> {
    let limit = Limit {
        bytes: 512 << 20,
        time: Duration::from_secs(15 * 60),
    };
    request("GET", url, &[], None, user_agent, 30_000, limit, true)
}

pub fn get_metadata(url: &str, user_agent: &str) -> Result<(u16, Vec<u8>)> {
    let limit = Limit {
        bytes: 1 << 20,
        time: Duration::from_secs(15),
    };
    request("GET", url, &[], None, user_agent, 5_000, limit, true)
}

pub fn post(
    url: &str,
    content_type: &str,
    body: &[u8],
    user_agent: &str,
    timeout_ms: i32,
) -> Result<(u16, Vec<u8>)> {
    let limit = Limit {
        bytes: 8 << 20,
        time: Duration::from_millis(timeout_ms.max(0) as u64 + 60_000),
    };
    request(
        "POST",
        url,
        &[],
        Some((content_type, body)),
        user_agent,
        timeout_ms,
        limit,
        true,
    )
}

pub fn call(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<(&str, &[u8])>,
    user_agent: &str,
    timeout_ms: i32,
) -> Result<(u16, Vec<u8>)> {
    let limit = Limit {
        bytes: 8 << 20,
        time: Duration::from_millis(timeout_ms.max(0) as u64 + 60_000),
    };
    request(
        method, url, headers, body, user_agent, timeout_ms, limit, false,
    )
}

pub fn sha256(data: &[u8]) -> Result<[u8; 32]> {
    Ok(sha2::Sha256::digest(data).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_refuses_plain_http() {
        let h = sha256(b"abc").unwrap();
        assert_eq!(h[0], 0xba);
        assert_eq!(h[31], 0xad);
        assert!(get("http://example.com/", "t").is_err());
    }
}
