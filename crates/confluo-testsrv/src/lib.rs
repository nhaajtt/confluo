//! A small HTTP file server that misbehaves on purpose.
//!
//! It serves deterministic pseudo-random content (so no fixture files are
//! needed and any byte can be checked) and can cap speed, refuse ranges,
//! drop or stall responses, corrupt a byte, change the file mid-download
//! and expire signed links.

use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const CHUNK: usize = 16 * 1024;

/// How the server behaves. Tests mutate this through [`Server::behavior`].
#[derive(Debug, Clone)]
pub struct Behavior {
    pub size: u64,
    pub seed: u64,
    /// Honour `Range` requests.
    pub ranges: bool,
    /// Send `Content-Length` (when false the body is chunked and the size unknown).
    pub content_length: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub filename: Option<String>,
    /// Bytes per second for each response.
    pub rate_per_conn: Option<u64>,
    /// Bytes per second shared by all responses to one client address.
    pub rate_per_ip: Option<u64>,
    /// Bytes per second shared by everything this server sends.
    pub rate_total: Option<u64>,
    /// Concurrent responses allowed; more get `503`.
    pub max_conns: Option<usize>,
    /// Delay before response headers.
    pub latency: Duration,
    /// Abort the connection after this many body bytes, for the next `drop_count` responses.
    pub drop_after: Option<u64>,
    pub drop_count: u32,
    /// Stop sending (but keep the connection open) after this many body bytes,
    /// for the next `stall_count` responses.
    pub stall_after: Option<u64>,
    pub stall_count: u32,
    /// Flip the byte at this absolute offset, for the next `corrupt_count` responses covering it.
    pub corrupt_at: Option<u64>,
    pub corrupt_count: u32,
    /// When set, `/file` needs `?t=<token>`; anything else gets `403`.
    /// `/link` always redirects to a URL carrying the current token.
    pub token: Option<String>,
    /// Answer with this status instead of serving, for the next `fail_count` requests.
    pub fail_status: Option<u16>,
    pub fail_count: u32,
    /// Value of the `Digest`-style header `Repr-Digest`/`Content-MD5` is not sent; this
    /// advertises a SHA-256 hex digest in `X-Checksum-Sha256` when set.
    pub sha256_hex: Option<String>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            size: 1 << 20,
            seed: 1,
            ranges: true,
            content_length: true,
            etag: Some("\"v1\"".into()),
            last_modified: None,
            filename: None,
            rate_per_conn: None,
            rate_per_ip: None,
            rate_total: None,
            max_conns: None,
            latency: Duration::ZERO,
            drop_after: None,
            drop_count: 0,
            stall_after: None,
            stall_count: 0,
            corrupt_at: None,
            corrupt_count: 0,
            token: None,
            fail_status: None,
            fail_count: 0,
            sha256_hex: None,
        }
    }
}

/// Counters a test can assert on.
#[derive(Debug, Default)]
pub struct Stats {
    pub requests: AtomicU64,
    pub range_requests: AtomicU64,
    pub body_bytes: AtomicU64,
    pub active: AtomicUsize,
    pub peak_active: AtomicUsize,
    pub connections: AtomicU64,
    pub by_client: Mutex<HashMap<IpAddr, u64>>,
}

impl Stats {
    pub fn bytes_from(&self, ip: IpAddr) -> u64 {
        self.by_client.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }
}

/// Fills `buf` with the content of the file `seed` starting at `offset`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    let mut word = offset / 8;
    let mut skip = (offset % 8) as usize;
    let mut i = 0;
    while i < buf.len() {
        let bytes = splitmix(seed ^ word.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_le_bytes();
        let n = (8 - skip).min(buf.len() - i);
        buf[i..i + n].copy_from_slice(&bytes[skip..skip + n]);
        i += n;
        skip = 0;
        word += 1;
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The whole content of a file, for comparisons in tests.
pub fn content(seed: u64, size: u64) -> Vec<u8> {
    let mut v = vec![0u8; size as usize];
    fill(seed, 0, &mut v);
    v
}

/// A pacing limiter: callers sleep until their bytes fit under the rate.
#[derive(Debug)]
struct Pacer {
    next: Mutex<Instant>,
}

impl Pacer {
    fn new() -> Self {
        Self { next: Mutex::new(Instant::now()) }
    }

    async fn take(&self, bytes: usize, rate: u64) {
        let cost = Duration::from_secs_f64(bytes as f64 / rate.max(1) as f64);
        let until = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            // Allow a small burst so pacing does not drift below the target.
            let start = (*next).max(now.checked_sub(Duration::from_millis(20)).unwrap_or(now));
            *next = start + cost;
            *next
        };
        tokio::time::sleep_until(until.into()).await;
    }
}

struct Shared {
    behavior: Mutex<Behavior>,
    stats: Stats,
    total: Pacer,
    per_ip: Mutex<HashMap<IpAddr, Arc<Pacer>>>,
}

/// A running test server. Dropping it closes the listener and every open connection.
pub struct Server {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    /// Starts a server on `bind` (use port 0 for any free port).
    pub async fn start(bind: SocketAddr, behavior: Behavior) -> io::Result<Self> {
        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            behavior: Mutex::new(behavior),
            stats: Stats::default(),
            total: Pacer::new(),
            per_ip: Mutex::new(HashMap::new()),
        });
        let accept_shared = shared.clone();
        let task = tokio::spawn(async move {
            // Connections live in this set, so stopping the server cuts every one of them.
            let mut conns = tokio::task::JoinSet::new();
            loop {
                let Ok((stream, peer)) = listener.accept().await else { continue };
                while conns.try_join_next().is_some() {}
                let _ = stream.set_nodelay(true);
                let shared = accept_shared.clone();
                shared.stats.connections.fetch_add(1, Ordering::Relaxed);
                conns.spawn(async move {
                    let svc = service_fn(move |req| handle(shared.clone(), peer, req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        Ok(Self { addr, shared, task })
    }

    pub async fn local(behavior: Behavior) -> Self {
        Self::start(([127, 0, 0, 1], 0).into(), behavior).await.expect("bind test server")
    }

    pub fn url(&self) -> String {
        format!("http://{}/file", self.addr)
    }

    /// A stable URL that redirects to the (possibly token-protected) file.
    pub fn link_url(&self) -> String {
        format!("http://{}/link", self.addr)
    }

    pub fn stats(&self) -> &Stats {
        &self.shared.stats
    }

    /// Changes behaviour while running.
    pub fn behavior<R>(&self, f: impl FnOnce(&mut Behavior) -> R) -> R {
        f(&mut self.shared.behavior.lock().unwrap())
    }
}

struct ChanBody {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    len: Option<u64>,
}

impl Body for ChanBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.rx.poll_recv(cx).map(|o| o.map(|r| r.map(Frame::data)))
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        match self.len {
            Some(n) => hyper::body::SizeHint::with_exact(n),
            None => hyper::body::SizeHint::default(),
        }
    }
}

fn empty(status: StatusCode) -> Response<ChanBody> {
    let (_, rx) = mpsc::channel(1);
    let mut r = Response::new(ChanBody { rx, len: Some(0) });
    *r.status_mut() = status;
    r
}

fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        return (n > 0 && size > 0).then(|| (size.saturating_sub(n), size - 1));
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() {
        size.saturating_sub(1)
    } else {
        b.parse::<u64>().ok()?.min(size.saturating_sub(1))
    };
    (start <= end && start < size).then_some((start, end))
}

struct ActiveGuard(Arc<Shared>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.stats.active.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn handle(
    shared: Arc<Shared>,
    peer: SocketAddr,
    req: Request<Incoming>,
) -> Result<Response<ChanBody>, Infallible> {
    let stats = &shared.stats;
    stats.requests.fetch_add(1, Ordering::Relaxed);
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    // Take a snapshot and consume one-shot fault counters under the lock.
    let (b, fail) = {
        let mut b = shared.behavior.lock().unwrap();
        let fail = if b.fail_count > 0 {
            b.fail_count -= 1;
            b.fail_status
        } else {
            None
        };
        (b.clone(), fail)
    };
    if !b.latency.is_zero() {
        tokio::time::sleep(b.latency).await;
    }
    if let Some(code) = fail {
        let mut r = empty(StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE));
        if code == 429 || code == 503 {
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        return Ok(r);
    }

    if let Some(rest) = path.strip_prefix("/redirect/") {
        let n: u32 = rest.parse().unwrap_or(0);
        let to = if n <= 1 { "/file".to_string() } else { format!("/redirect/{}", n - 1) };
        let mut r = empty(StatusCode::FOUND);
        r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&to).unwrap());
        return Ok(r);
    }
    if path == "/link" {
        let to = match &b.token {
            Some(t) => format!("/file?t={t}"),
            None => "/file".to_string(),
        };
        let mut r = empty(StatusCode::FOUND);
        r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&to).unwrap());
        return Ok(r);
    }
    if path == "/stats" || path == "/reset" {
        // Counters for benchmarks: how much the server really sent, in how many requests.
        stats.requests.fetch_sub(1, Ordering::Relaxed);
        let body = format!(
            "{{\"requests\":{},\"range_requests\":{},\"body_bytes\":{},\"peak_active\":{},\"connections\":{}}}",
            stats.requests.load(Ordering::Relaxed),
            stats.range_requests.load(Ordering::Relaxed),
            stats.body_bytes.load(Ordering::Relaxed),
            stats.peak_active.load(Ordering::Relaxed),
            stats.connections.load(Ordering::Relaxed).saturating_sub(1),
        );
        if path == "/reset" {
            stats.requests.store(0, Ordering::Relaxed);
            stats.range_requests.store(0, Ordering::Relaxed);
            stats.body_bytes.store(0, Ordering::Relaxed);
            stats.peak_active.store(0, Ordering::Relaxed);
            stats.connections.store(0, Ordering::Relaxed);
        }
        let (tx, rx) = mpsc::channel(1);
        let len = body.len() as u64;
        let _ = tx.try_send(Ok(Bytes::from(body)));
        let mut r = Response::new(ChanBody { rx, len: Some(len) });
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        return Ok(r);
    }
    if !path.starts_with("/file") {
        return Ok(empty(StatusCode::NOT_FOUND));
    }
    if let Some(token) = &b.token {
        let ok = query.split('&').any(|kv| kv.strip_prefix("t=") == Some(token.as_str()));
        if !ok {
            return Ok(empty(StatusCode::FORBIDDEN));
        }
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Ok(empty(StatusCode::METHOD_NOT_ALLOWED));
    }

    let active = stats.active.fetch_add(1, Ordering::Relaxed) + 1;
    let guard = ActiveGuard(shared.clone());
    stats.peak_active.fetch_max(active, Ordering::Relaxed);
    if b.max_conns.is_some_and(|m| active > m) {
        let mut r = empty(StatusCode::SERVICE_UNAVAILABLE);
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return Ok(r);
    }

    let range_header = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    // `If-Range` with a validator that no longer matches means "send the whole file".
    let if_range_ok = match req.headers().get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
        Some(v) => b.etag.as_deref() == Some(v) || b.last_modified.as_deref() == Some(v),
        None => true,
    };
    let mut status = StatusCode::OK;
    let (mut start, mut end) = (0u64, b.size.saturating_sub(1));
    if let (true, true, Some(h)) = (b.ranges, if_range_ok, range_header) {
        stats.range_requests.fetch_add(1, Ordering::Relaxed);
        match parse_range(h, b.size) {
            Some((s, e)) => {
                start = s;
                end = e;
                status = StatusCode::PARTIAL_CONTENT;
            }
            None => {
                let mut r = empty(StatusCode::RANGE_NOT_SATISFIABLE);
                r.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", b.size)).unwrap(),
                );
                return Ok(r);
            }
        }
    }
    let len = if b.size == 0 { 0 } else { end - start + 1 };

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(4);
    let mut resp = Response::new(ChanBody { rx, len: b.content_length.then_some(len) });
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    if b.ranges {
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", b.size)).unwrap(),
        );
    }
    if let Some(v) = b.etag.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        h.insert(header::ETAG, v);
    }
    if let Some(v) = b.last_modified.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        h.insert(header::LAST_MODIFIED, v);
    }
    if let Some(v) = b.sha256_hex.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        h.insert("x-checksum-sha256", v);
    }
    if let Some(name) = &b.filename
        && let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));

    if req.method() == Method::HEAD || len == 0 {
        return Ok(resp);
    }

    // Consume one-shot body faults.
    let (drop_after, stall_after, corrupt_at) = {
        let mut live = shared.behavior.lock().unwrap();
        let drop_after = (live.drop_count > 0).then_some(live.drop_after).flatten();
        if drop_after.is_some_and(|n| n < len) {
            live.drop_count -= 1;
        }
        let stall_after = (live.stall_count > 0).then_some(live.stall_after).flatten();
        if stall_after.is_some_and(|n| n < len) {
            live.stall_count -= 1;
        }
        let corrupt_at = (live.corrupt_count > 0)
            .then_some(live.corrupt_at)
            .flatten()
            .filter(|o| (start..=end).contains(o));
        if corrupt_at.is_some() {
            live.corrupt_count -= 1;
        }
        (drop_after.filter(|n| *n < len), stall_after.filter(|n| *n < len), corrupt_at)
    };

    let ip_pacer = b.rate_per_ip.map(|_| {
        shared.per_ip.lock().unwrap().entry(peer.ip()).or_insert_with(|| Arc::new(Pacer::new())).clone()
    });
    let producer = shared.clone();
    tokio::spawn(async move {
        let _guard = guard;
        let conn_pacer = Pacer::new();
        let mut sent = 0u64;
        while sent < len {
            let mut n = CHUNK.min((len - sent) as usize);
            if let Some(limit) = drop_after {
                if sent >= limit {
                    let _ = tx.send(Err(io::Error::new(io::ErrorKind::ConnectionReset, "dropped"))).await;
                    return;
                }
                n = n.min((limit - sent) as usize);
            }
            if let Some(limit) = stall_after {
                if sent >= limit {
                    tx.closed().await;
                    return;
                }
                n = n.min((limit - sent) as usize);
            }
            let mut buf = vec![0u8; n];
            let pos = start + sent;
            fill(b.seed, pos, &mut buf);
            if let Some(o) = corrupt_at
                && (pos..pos + n as u64).contains(&o)
            {
                buf[(o - pos) as usize] ^= 0xFF;
            }
            if let Some(r) = b.rate_per_conn {
                conn_pacer.take(n, r).await;
            }
            if let (Some(r), Some(p)) = (b.rate_per_ip, &ip_pacer) {
                p.take(n, r).await;
            }
            if let Some(r) = b.rate_total {
                producer.total.take(n, r).await;
            }
            if tx.send(Ok(Bytes::from(buf))).await.is_err() {
                return;
            }
            sent += n as u64;
            producer.stats.body_bytes.fetch_add(n as u64, Ordering::Relaxed);
            *producer.stats.by_client.lock().unwrap().entry(peer.ip()).or_insert(0) += n as u64;
        }
    });
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_is_offset_consistent() {
        let all = content(7, 1000);
        for off in [0u64, 1, 7, 8, 9, 123, 999] {
            let mut part = vec![0u8; (1000 - off).min(37) as usize];
            fill(7, off, &mut part);
            assert_eq!(part, all[off as usize..off as usize + part.len()]);
        }
    }

    #[test]
    fn ranges_parse() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
    }
}
