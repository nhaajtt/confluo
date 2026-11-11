//! Asking a server what a link points to and what it supports.

use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Response, StatusCode};
use url::Url;
use confluo_net::BindSpec;

use crate::error::{Error, Result};
use crate::http::{self, Ask, Conn, Fetched, RequestOptions};
use crate::name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The URL after redirects.
    pub url: Url,
    pub size: Option<u64>,
    /// The server answers byte-range requests correctly.
    pub ranges: bool,
    /// Strong ETag as sent with the probe response.
    pub etag: Option<String>,
    /// Whether the ETag was seen to be the same for two different ranges. Some
    /// servers compute an ETag per range; comparing those across requests would
    /// look like the file changing.
    pub etag_checked: bool,
    pub last_modified: Option<String>,
    pub file_name: Option<String>,
    /// SHA-256 advertised by the server, lowercase hex.
    pub sha256: Option<String>,
}

/// A response that is already delivering the file from byte 0, with its connection.
pub struct Head {
    pub conn: Conn,
    pub response: Response<Incoming>,
}

pub(crate) fn strong_etag(v: Option<&str>) -> Option<String> {
    let v = v?.trim();
    (!v.starts_with("W/") && v.len() >= 2).then(|| v.to_string())
}

fn advertised_sha256(resp: &Response<Incoming>) -> Option<String> {
    use base64::Engine as _;
    let hex_ok = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    for name in ["x-checksum-sha256", "x-amz-meta-sha256", "x-goog-meta-sha256"] {
        if let Some(v) = http::header_str(resp, name).map(str::trim).filter(|v| hex_ok(v)) {
            return Some(v.to_ascii_lowercase());
        }
    }
    // RFC 9530 `Repr-Digest: sha-256=:base64:` and the older `Digest: sha-256=base64`.
    for name in ["repr-digest", "digest"] {
        let Some(v) = http::header_str(resp, name) else { continue };
        for item in v.split(',') {
            let Some((alg, val)) = item.trim().split_once('=') else { continue };
            if !alg.trim().eq_ignore_ascii_case("sha-256") {
                continue;
            }
            let raw = val.trim().trim_matches(':');
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(raw)
                && bytes.len() == 32
            {
                return Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
            }
        }
    }
    None
}

/// Sends the probing GET. Many file hosts answer HEAD differently from GET, so
/// a ranged GET is used; hosts that reject ranges outright get a plain GET.
async fn ask(
    slot: &mut Option<Conn>,
    bind: Option<&BindSpec>,
    url: &Url,
    opts: &RequestOptions,
    last: Option<u64>,
) -> Result<Fetched> {
    let fetched = http::get(slot, bind, url, opts, &Ask { range: Some((0, last)), if_range: None }).await?;
    if matches!(fetched.response.status().as_u16(), 400 | 403 | 405 | 501) {
        *slot = None;
        return http::get(slot, bind, url, opts, &Ask::default()).await;
    }
    Ok(fetched)
}

/// Reads what the response says about the file. `last` is the last byte asked for.
fn describe(requested: &Url, fetched: &Fetched, last: Option<u64>) -> Result<Probe> {
    let resp = &fetched.response;
    let status = resp.status();
    let file_name = http::header_str(resp, "content-disposition")
        .and_then(name::from_content_disposition)
        .or_else(|| name::from_url(&fetched.url))
        .or_else(|| name::from_url(requested));
    let mut out = Probe {
        url: fetched.url.clone(),
        size: None,
        ranges: false,
        etag: strong_etag(http::header_str(resp, "etag")),
        etag_checked: false,
        last_modified: http::header_str(resp, "last-modified").map(str::to_owned),
        file_name,
        sha256: advertised_sha256(resp),
    };
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // `bytes */0`: an empty file cannot satisfy any range.
        let total =
            http::header_str(resp, "content-range").and_then(|v| v.rsplit('/').next()?.trim().parse().ok());
        if total == Some(0u64) {
            out.size = Some(0);
            return Ok(out);
        }
    }
    http::check_status(resp)?;
    if status == StatusCode::PARTIAL_CONTENT {
        match http::content_range(resp) {
            Some((0, b, Some(total))) if total > 0 && b == last.unwrap_or(total - 1).min(total - 1) => {
                out.size = Some(total);
                out.ranges = true;
            }
            // A 206 that does not describe what was asked for cannot be trusted.
            _ => return Err(Error::Protocol("unusable Content-Range in probe".into())),
        }
    } else {
        out.size = http::content_length(resp);
    }
    Ok(out)
}

/// Reads a (tiny) response body to its end so the connection can be used again.
async fn drain(resp: Response<Incoming>) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(2), resp.into_body().collect()).await, Ok(Ok(_)))
}

/// Probes `url` through `bind` by asking for its first byte, then a second
/// byte to learn whether the ETag can be trusted across ranges.
pub async fn probe(bind: Option<&BindSpec>, url: &Url, opts: &RequestOptions) -> Result<Probe> {
    let mut slot = None;
    let fetched = ask(&mut slot, bind, url, opts, Some(0)).await?;
    let mut out = describe(url, &fetched, Some(0))?;
    if !out.ranges {
        return Ok(out);
    }
    if !drain(fetched.response).await {
        slot = None;
    }
    if out.etag.is_some() && out.size.is_some_and(|s| s > 1) {
        let second = Ask { range: Some((1, Some(1))), if_range: None };
        match http::get(&mut slot, bind, &out.url, opts, &second).await {
            Ok(f) if strong_etag(http::header_str(&f.response, "etag")) == out.etag => {
                out.etag_checked = true
            }
            Ok(_) => out.etag = None,
            Err(_) => {}
        }
    }
    Ok(out)
}

/// Probes `url` by asking for the whole file from byte 0. The answer both
/// describes the file and *is* the first data stream, so no time is spent on a
/// separate round trip before the download starts.
pub async fn probe_stream(
    bind: Option<&BindSpec>,
    url: &Url,
    opts: &RequestOptions,
) -> Result<(Probe, Option<Head>)> {
    let mut slot = None;
    let fetched = ask(&mut slot, bind, url, opts, None).await?;
    let out = describe(url, &fetched, None)?;
    let head = match (slot, fetched.response.status()) {
        (Some(conn), StatusCode::OK | StatusCode::PARTIAL_CONTENT) if out.size != Some(0) => {
            Some(Head { conn, response: fetched.response })
        }
        _ => None,
    };
    Ok((out, head))
}
