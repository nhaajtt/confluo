//! Asking a server what a link points to and what it supports.

use hyper::StatusCode;
use url::Url;
use weft_net::BindSpec;

use crate::error::{Error, Result};
use crate::http::{self, Ask, RequestOptions};
use crate::name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The URL after redirects.
    pub url: Url,
    pub size: Option<u64>,
    /// The server answers byte-range requests correctly.
    pub ranges: bool,
    /// Strong ETag, only when the server gives the same one for every range.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub file_name: Option<String>,
    /// SHA-256 advertised by the server, lowercase hex.
    pub sha256: Option<String>,
}

impl Probe {
    /// The validator to send in `If-Range`.
    pub fn validator(&self) -> Option<&str> {
        self.etag.as_deref().or(self.last_modified.as_deref())
    }
}

fn strong_etag(v: Option<&str>) -> Option<String> {
    let v = v?.trim();
    (!v.starts_with("W/") && v.len() >= 2).then(|| v.to_string())
}

fn advertised_sha256(resp: &hyper::Response<hyper::body::Incoming>) -> Option<String> {
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

/// Probes `url` through `bind`. Uses a one-byte ranged GET rather than HEAD,
/// because many file hosts answer HEAD differently from GET.
pub async fn probe(bind: Option<&BindSpec>, url: &Url, opts: &RequestOptions) -> Result<Probe> {
    let mut slot = None;
    let ask = Ask { range: Some((0, Some(0))), if_range: None };
    let mut fetched = http::get(&mut slot, bind, url, opts, &ask).await?;
    let mut status = fetched.response.status();
    if matches!(status.as_u16(), 400 | 403 | 405 | 501) {
        // Some hosts reject ranged requests outright but serve the plain file.
        slot = None;
        fetched = http::get(&mut slot, bind, url, opts, &Ask::default()).await?;
        status = fetched.response.status();
    }
    let resp = &fetched.response;
    let file_name = http::header_str(resp, "content-disposition")
        .and_then(name::from_content_disposition)
        .or_else(|| name::from_url(&fetched.url))
        .or_else(|| name::from_url(url));
    let mut out = Probe {
        url: fetched.url.clone(),
        size: None,
        ranges: false,
        etag: strong_etag(http::header_str(resp, "etag")),
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
            Some((0, 0, Some(total))) => {
                out.size = Some(total);
                out.ranges = true;
            }
            // A 206 that does not describe the byte we asked for cannot be trusted.
            _ => return Err(Error::Protocol("unusable Content-Range in probe".into())),
        }
    } else {
        out.size = http::content_length(resp);
    }
    if out.ranges && out.etag.is_some() && out.size.is_some_and(|s| s > 1) {
        // Some servers compute an ETag per range. Comparing those across requests
        // would look like the file changing, so such ETags are ignored.
        drop(fetched);
        slot = None;
        let ask = Ask { range: Some((1, Some(1))), if_range: None };
        if let Ok(second) = http::get(&mut slot, bind, &out.url, opts, &ask).await
            && strong_etag(http::header_str(&second.response, "etag")) != out.etag
        {
            out.etag = None;
        }
    }
    Ok(out)
}
