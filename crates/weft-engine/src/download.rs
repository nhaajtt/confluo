//! One download from start to finish: probe, plan, fetch over every network, recover, verify.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::BytesMut;
use http_body_util::BodyExt;
use hyper::StatusCode;
use serde::Serialize;
use tokio::sync::watch;
use tokio::task::{AbortHandle, Id, JoinSet};
use url::Url;
use weft_net::BindSpec;

use crate::disk::{self, DiskWriter};
use crate::error::{Error, Result};
use crate::hash::{self, Algorithm, Checksum};
use crate::http::{self, Ask, Conn, RequestOptions};
use crate::journal::{self, Journal};
use crate::limit::{Limiter, RateMeter};
use crate::name;
use crate::probe::{Head, Probe, probe, probe_stream};
use crate::ranges::RangeSet;
use crate::sched::{Lease, SchedConfig, Scheduler};
use crate::tuner::Tuner;

/// Stand-in end offset while the size of a file is not known.
const UNKNOWN_END: u64 = u64::MAX / 4;
const SAMPLE_LEN: u64 = 4096;
const ETAG_UNKNOWN: u8 = 0;
const ETAG_TRUSTED: u8 = 1;
const ETAG_IGNORED: u8 = 2;
/// How long a connection with nothing to do stays open waiting for work.
const PARK_FOR: Duration = Duration::from_secs(4);

/// A way out to the internet: one network interface (or the OS default route).
#[derive(Debug, Clone)]
pub struct Route {
    pub id: String,
    pub label: String,
    /// `None` lets the operating system pick the interface.
    pub bind: Option<BindSpec>,
    pub limiter: Arc<Limiter>,
    /// Routes with the same uplink key share one internet connection
    /// (for example Wi-Fi and Ethernet to the same router).
    pub uplink: Option<String>,
}

impl Route {
    /// The operating system's default route.
    pub fn system() -> Self {
        Self {
            id: "default".into(),
            label: "System default".into(),
            bind: None,
            limiter: Arc::default(),
            uplink: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Spec {
    /// The primary link first, then optional mirrors of the same file.
    pub urls: Vec<Url>,
    pub dir: PathBuf,
    /// Overrides the name the server suggests.
    pub file_name: Option<String>,
    pub headers: Vec<(String, String)>,
    pub checksum: Option<Checksum>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub header_timeout: Duration,
    /// A connection that delivers nothing for this long is replaced.
    pub stall_timeout: Duration,
    /// Connections each network starts with.
    pub conns_initial: usize,
    /// Most connections one network may open.
    pub conns_max: usize,
    /// Most connections for the whole download.
    pub conns_total: usize,
    pub sched: SchedConfig,
    /// Bytes gathered per connection before they are handed to the disk thread.
    pub write_chunk: usize,
    pub journal_interval: Duration,
    pub tick: Duration,
    /// Fail when nothing arrived for this long despite retries. `None` waits forever.
    pub give_up_after: Option<Duration>,
    /// Limit shared by every route of this download (and possibly other downloads).
    pub limiter: Arc<Limiter>,
    /// Resolution of the progress map in snapshots.
    pub map_cells: usize,
}

impl Default for Config {
    fn default() -> Self {
        let r = RequestOptions::default();
        Self {
            user_agent: r.user_agent,
            connect_timeout: r.connect_timeout,
            header_timeout: r.header_timeout,
            stall_timeout: Duration::from_secs(20),
            conns_initial: 8,
            conns_max: 32,
            conns_total: 64,
            sched: SchedConfig::default(),
            write_chunk: 256 * 1024,
            journal_interval: Duration::from_secs(1),
            tick: Duration::from_millis(250),
            give_up_after: None,
            limiter: Arc::default(),
            map_cells: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    /// Stop and keep everything needed to continue later.
    Pause,
    /// Stop and delete the partial file.
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed { path: PathBuf, size: u64 },
    Paused,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Probing,
    Downloading,
    /// Requests are failing; retrying with back-off.
    Retrying,
    /// No usable network interface right now.
    WaitingForNetwork,
    Verifying,
}

/// The single thing that currently holds the speed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Nothing else stands out: the network path itself.
    Network,
    /// The server caps each connection; more connections keep helping.
    ServerPerConnection,
    /// The server refuses more connections or requests.
    ServerRefusing,
    /// The server cannot resume or split this file: one connection only.
    ServerSingleStream,
    Disk,
    UserLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteHealth {
    Active,
    Idle,
    Retrying,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteSnapshot {
    pub id: String,
    pub label: String,
    pub bytes: u64,
    pub rate: f64,
    pub connections: usize,
    pub target: usize,
    pub health: RouteHealth,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub phase: Phase,
    pub file_name: String,
    pub size: Option<u64>,
    pub done: u64,
    pub rate: f64,
    pub eta_secs: Option<f64>,
    pub connections: usize,
    pub routes: Vec<RouteSnapshot>,
    /// How complete each slice of the file is, 0..=255.
    pub map: Vec<u8>,
    pub verdict: Verdict,
    /// Two or more active routes share one internet connection, so adding them up gains nothing.
    pub shared_uplink: bool,
    /// False when the server cannot resume: an interruption restarts from zero.
    pub resumable: bool,
    pub sources: usize,
    pub steals: u64,
    /// How long the last flush of the partial file to disk took, in milliseconds.
    pub flush_ms: u32,
    pub notes: Vec<String>,
}

struct Source {
    requested: Url,
    current: Mutex<Url>,
    etag: Option<String>,
    last_modified: Option<String>,
    active: AtomicUsize,
    dead: AtomicBool,
    /// Counts link refreshes, so a failure can tell whether it used the old link.
    generation: AtomicU64,
    /// Whether the ETag may be compared across ranges: `ETAG_UNKNOWN` until two
    /// different ranges have been seen, then trusted or ignored for good.
    etag_state: AtomicU8,
}

impl Source {
    fn new(requested: Url, p: &Probe) -> Self {
        Self {
            requested,
            current: Mutex::new(p.url.clone()),
            etag: p.etag.clone(),
            last_modified: p.last_modified.clone(),
            active: AtomicUsize::new(0),
            dead: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            etag_state: AtomicU8::new(if p.etag_checked { ETAG_TRUSTED } else { ETAG_UNKNOWN }),
        }
    }

    /// The validator for `If-Range`: the ETag once it is known to name the file, else the date.
    fn validator(&self) -> Option<&str> {
        let etag = self.etag.as_deref().filter(|_| self.etag_state.load(Ordering::Acquire) == ETAG_TRUSTED);
        etag.or(self.last_modified.as_deref())
    }
}

struct RouteShared {
    route: Route,
    bytes: AtomicU64,
    active: AtomicUsize,
    target: AtomicUsize,
}

struct Ctx {
    cfg: Config,
    opts: RequestOptions,
    sched: Mutex<Scheduler>,
    writer: DiskWriter,
    sources: Vec<Source>,
    size: Option<u64>,
    /// Size learned at end of stream when the server did not announce one.
    final_size: AtomicU64,
    /// The server cannot serve ranges: one connection, from byte 0.
    whole: bool,
    /// The connection the probe used, still open, for whichever worker starts first on that route.
    spare: Mutex<Vec<(Option<BindSpec>, Conn)>>,
}

/// Keeps the per-route and per-source connection counts right however a worker ends.
struct ActiveGuard {
    ctx: Arc<Ctx>,
    route: Arc<RouteShared>,
    source: usize,
}

impl ActiveGuard {
    fn new(ctx: Arc<Ctx>, route: Arc<RouteShared>, source: usize) -> Self {
        route.active.fetch_add(1, Ordering::AcqRel);
        ctx.sources[source].active.fetch_add(1, Ordering::AcqRel);
        Self { ctx, route, source }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.route.active.fetch_sub(1, Ordering::AcqRel);
        self.ctx.sources[self.source].active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Returns a lease to the scheduler however the fetch ends (including task abort).
struct LeaseGuard<'a> {
    ctx: &'a Ctx,
    id: u64,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        self.ctx.sched.lock().unwrap().release(self.id);
    }
}

enum Exit {
    /// Nothing left to fetch.
    NoWork,
    /// The route has more connections than it should.
    Surplus,
    Failed {
        source: usize,
        /// The link generation the failed request used.
        generation: u64,
        error: Error,
    },
}

/// One connection: take a lease, fetch it, repeat. `first` is a lease whose
/// response is already arriving (the request the probe made).
async fn worker(guard: ActiveGuard, mut first: Option<(Lease, Head)>) -> Exit {
    let ctx = guard.ctx.clone();
    let route = guard.route.clone();
    let source = guard.source;
    let mut conn: Option<Conn> = if first.is_some() {
        None
    } else {
        let mut spare = ctx.spare.lock().unwrap();
        let found = spare.iter().position(|(bind, _)| *bind == route.route.bind);
        found.map(|i| spare.swap_remove(i).1)
    };
    let mut meter = RateMeter::new(Duration::from_millis(500), Instant::now());
    let mut idle_since: Option<Instant> = None;
    loop {
        let (lease, head) = if let Some((lease, head)) = first.take() {
            (lease, Some(head))
        } else {
            if route.active.load(Ordering::Acquire) > route.target.load(Ordering::Acquire) {
                return Exit::Surplus;
            }
            if ctx.sources[source].dead.load(Ordering::Acquire) {
                return Exit::NoWork;
            }
            let rate = meter.rate(Instant::now());
            let Some(lease) = ctx.sched.lock().unwrap().acquire(rate) else {
                // Nothing to do right now, but a lease may come back (a failed connection,
                // a slow one worth splitting). Wait with the connection still warm rather
                // than paying for a new handshake later.
                let since = *idle_since.get_or_insert_with(Instant::now);
                if conn.is_none() || since.elapsed() > PARK_FOR {
                    return Exit::NoWork;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            (lease, None)
        };
        idle_since = None;
        let generation = ctx.sources[source].generation.load(Ordering::Acquire);
        let release = LeaseGuard { ctx: &ctx, id: lease.id };
        let result = fetch(&ctx, &route, source, &mut conn, &lease, &mut meter, head).await;
        if ctx.whole && result.is_err() {
            // No ranges: whatever arrived is useless, the next attempt starts at byte 0.
            let _ = ctx.writer.truncate().await;
            lease.pos.store(lease.start, Ordering::Release);
        }
        drop(release);
        match result {
            Ok(true) => {}
            Ok(false) => conn = None,
            Err(error) => return Exit::Failed { source, generation, error },
        }
    }
}

fn strong(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|v| !v.starts_with("W/"))
}

/// Fetches one lease. Returns whether the connection can serve another request.
async fn fetch(
    ctx: &Ctx,
    route: &RouteShared,
    source: usize,
    conn: &mut Option<Conn>,
    lease: &Lease,
    meter: &mut RateMeter,
    head: Option<Head>,
) -> Result<bool> {
    let src = &ctx.sources[source];
    let url = src.current.lock().unwrap().clone();
    let start = lease.start;
    let want_end = lease.end();
    let ask = if ctx.whole {
        Ask::default()
    } else {
        Ask { range: Some((start, Some(want_end - 1))), if_range: src.validator() }
    };
    let from_probe = head.is_some();
    let resp = match head {
        // The probe asked for everything from byte 0 and its answer is still arriving.
        Some(head) => {
            *conn = Some(head.conn);
            head.response
        }
        None => http::get(conn, route.route.bind.as_ref(), &url, &ctx.opts, &ask).await?.response,
    };
    http::check_status(&resp)?;
    let mut resp_end = UNKNOWN_END;
    if ctx.whole {
        if resp.status() != StatusCode::OK {
            return Err(Error::Protocol(format!("expected 200, got {}", resp.status())));
        }
        if let (Some(len), Some(size)) = (http::content_length(&resp), ctx.size)
            && len != size
        {
            return Err(Error::FileChanged);
        }
    } else {
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            // With If-Range, a 200 means the validator no longer matches.
            return Err(if src.validator().is_some() {
                Error::FileChanged
            } else {
                Error::Protocol("server ignored the range request".into())
            });
        }
        let Some((a, b, total)) = http::content_range(&resp) else {
            return Err(Error::Protocol("206 without Content-Range".into()));
        };
        if a != start || b < a {
            return Err(Error::Protocol(format!("asked for byte {start}, server sent {a}-{b}")));
        }
        if let (Some(total), Some(size)) = (total, ctx.size)
            && total != size
        {
            return Err(Error::FileChanged);
        }
        if let (Some(ours), Some(theirs)) = (src.etag.as_deref(), strong(http::header_str(&resp, "etag")))
            && !from_probe
        {
            match src.etag_state.load(Ordering::Acquire) {
                ETAG_TRUSTED if ours != theirs => return Err(Error::FileChanged),
                // The first answer for a different range shows whether this ETag names
                // the file (comparable from now on) or the range (never comparable).
                ETAG_UNKNOWN => {
                    let verdict = if ours == theirs { ETAG_TRUSTED } else { ETAG_IGNORED };
                    src.etag_state.store(verdict, Ordering::Release);
                }
                _ => {}
            }
        }
        if let (Some(ours), Some(theirs)) =
            (src.last_modified.as_deref(), http::header_str(&resp, "last-modified"))
            && ours != theirs
        {
            return Err(Error::FileChanged);
        }
        resp_end = b + 1;
    }

    let mut body = resp.into_body();
    let chunk = ctx.cfg.write_chunk.max(16 * 1024);
    let mut buf = BytesMut::with_capacity(chunk + 64 * 1024);
    let mut buf_at = start;
    let mut received = start;
    let mut clean_eof = false;

    // Hands the buffered bytes to the disk thread, never past the lease's current end
    // (it moves down when another connection takes over the tail).
    async fn flush(ctx: &Ctx, lease: &Lease, buf: &mut BytesMut, buf_at: &mut u64, rate: f64) -> Result<()> {
        let mut data = buf.split().freeze();
        let keep = lease.end().saturating_sub(*buf_at).min(data.len() as u64) as usize;
        data.truncate(keep);
        if keep > 0 {
            ctx.writer.write(*buf_at, data).await.map_err(Error::Disk)?;
            *buf_at += keep as u64;
            lease.pos.store(*buf_at, Ordering::Release);
        }
        ctx.sched.lock().unwrap().set_rate(lease.id, rate);
        Ok(())
    }

    let streamed: Result<()> = async {
        loop {
            let limit = lease.end().min(resp_end);
            if received >= limit {
                // The response goes on past this lease. If the next bytes are still
                // unassigned, keep reading them from this response: no new request.
                if received < resp_end
                    && received == lease.end()
                    && ctx.sched.lock().unwrap().extend(lease.id)
                {
                    continue;
                }
                break;
            }
            let frame = match tokio::time::timeout(ctx.cfg.stall_timeout, body.frame()).await {
                Err(_) => return Err(Error::Timeout("data")),
                Ok(None) => {
                    clean_eof = true;
                    break;
                }
                Ok(Some(Err(e))) => return Err(e.into()),
                Ok(Some(Ok(f))) => f,
            };
            let Ok(mut data) = frame.into_data() else { continue };
            // A frame can reach past the lease. Bytes cut off here are gone from the
            // stream for good, so the lease has to grow first if this response is to
            // be read any further.
            let mut limit = limit;
            while received + data.len() as u64 > limit
                && limit < resp_end
                && limit == lease.end()
                && ctx.sched.lock().unwrap().extend(lease.id)
            {
                limit = lease.end().min(resp_end);
            }
            data.truncate((limit - received).min(data.len() as u64) as usize);
            let n = data.len();
            if n == 0 {
                continue;
            }
            ctx.cfg.limiter.take(n).await;
            route.route.limiter.take(n).await;
            route.bytes.fetch_add(n as u64, Ordering::Relaxed);
            meter.add(n as u64, Instant::now());
            received += n as u64;
            buf.extend_from_slice(&data);
            if buf.len() >= chunk {
                flush(ctx, lease, &mut buf, &mut buf_at, meter.rate(Instant::now())).await?;
            }
        }
        Ok(())
    }
    .await;
    // Keep what arrived even when the stream broke: those bytes need not be fetched again.
    let flushed = flush(ctx, lease, &mut buf, &mut buf_at, meter.rate(Instant::now())).await;
    streamed?;
    flushed?;

    if ctx.whole && ctx.size.is_none() {
        // Without a declared length, end of stream is the only end there is.
        ctx.final_size.store(received, Ordering::Release);
        lease.end.store(received, Ordering::Release);
        return Ok(false);
    }
    let limit = lease.end().min(resp_end);
    if received < limit {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed before the range was complete",
        )));
    }
    if received < resp_end {
        // Stopped early on purpose (the tail was taken over); the rest of this
        // response is still in flight, so the connection is not reusable.
        return Ok(false);
    }
    if !clean_eof {
        // Let the HTTP layer see the end of the body so the connection can be reused.
        clean_eof = matches!(tokio::time::timeout(Duration::from_millis(200), body.frame()).await, Ok(None));
    }
    Ok(clean_eof)
}

async fn fetch_bytes(
    bind: Option<&BindSpec>,
    url: &Url,
    opts: &RequestOptions,
    start: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let mut slot = None;
    let ask = Ask { range: Some((start, Some(start + len - 1))), if_range: None };
    let fetched = http::get(&mut slot, bind, url, opts, &ask).await?;
    http::check_status(&fetched.response)?;
    if fetched.response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(Error::Protocol("no range support".into()));
    }
    let body = tokio::time::timeout(opts.header_timeout, fetched.response.into_body().collect())
        .await
        .map_err(|_| Error::Timeout("sample"))??;
    let bytes = body.to_bytes();
    if bytes.len() as u64 != len {
        return Err(Error::Protocol("short sample".into()));
    }
    Ok(bytes.to_vec())
}

/// Checks that `mirror` serves the same bytes as `primary` at three places.
async fn same_content(
    bind: Option<&BindSpec>,
    primary: &Probe,
    mirror: &Probe,
    opts: &RequestOptions,
) -> Result<bool> {
    let Some(size) = primary.size else { return Ok(false) };
    if mirror.size != Some(size) || !mirror.ranges {
        return Ok(false);
    }
    if let (Some(a), Some(b)) = (&primary.sha256, &mirror.sha256) {
        return Ok(a == b);
    }
    let len = SAMPLE_LEN.min(size);
    for start in [0, (size - len) / 2, size - len] {
        let (a, b) = tokio::join!(
            fetch_bytes(bind, &primary.url, opts, start, len),
            fetch_bytes(bind, &mirror.url, opts, start, len)
        );
        if a? != b? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn jitter(base: Duration) -> Duration {
    let r = std::collections::hash_map::RandomState::new().build_hasher().finish();
    base.mul_f64(0.8 + (r % 1000) as f64 / 2500.0)
}

/// Back-off after the server said "too many requests": longer, because every
/// retry is itself a request that counts against the limit.
fn refusal_backoff(failures: u32) -> Duration {
    jitter(Duration::from_secs(2u64.saturating_mul(1 << failures.min(5)).min(60)))
}

fn backoff(failures: u32) -> Duration {
    jitter(Duration::from_millis(500u64.saturating_mul(1 << failures.min(5)).min(15_000)))
}

struct RouteRun {
    shared: Arc<RouteShared>,
    tuner: Tuner,
    meter: RateMeter,
    last_bytes: u64,
    failures: u32,
    /// When the last failure was counted, and how much the route had delivered then.
    failed_at: Option<(Instant, u64)>,
    next_spawn: Instant,
    error: Option<String>,
}

/// Either the receiver's next change, or never (once the sender is gone).
async fn changed<T>(rx: &mut watch::Receiver<T>, closed: &mut bool) {
    if *closed {
        return std::future::pending().await;
    }
    if rx.changed().await.is_err() {
        *closed = true;
        std::future::pending::<()>().await;
    }
}

struct Paths {
    name: String,
    part: PathBuf,
    journal: PathBuf,
}

/// Picks the file to write to: an interrupted download of the same link if there
/// is one, otherwise a name that collides with nothing.
fn plan_paths(spec: &Spec, p: &Probe) -> (Paths, Option<Journal>) {
    let base = name::sanitize(spec.file_name.as_deref().or(p.file_name.as_deref()).unwrap_or("download"));
    let part = name::part_path(&spec.dir, &base);
    let jpath = journal::journal_path(&part);
    if let Some(j) = journal::load(&jpath) {
        let same_link = j.urls.first().map(String::as_str) == spec.urls.first().map(Url::as_str);
        let same_etag = match (&j.etag, &p.etag) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let same_date = match (&j.last_modified, &p.last_modified) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let part_ok = std::fs::metadata(&part).is_ok_and(|m| p.size.is_none_or(|s| m.len() == s));
        if same_link && j.size == p.size && p.size.is_some() && p.ranges && same_etag && same_date && part_ok
        {
            return (Paths { name: base, part, journal: jpath }, Some(j));
        }
    }
    let name = name::unique(&spec.dir, &base, |f| {
        f.exists()
            || f.file_name().and_then(|n| n.to_str()).is_some_and(|n| name::part_path(&spec.dir, n).exists())
    });
    let part = name::part_path(&spec.dir, &name);
    let journal = journal::journal_path(&part);
    (Paths { name, part, journal }, None)
}

/// Runs one download until it completes, is paused or cancelled, or fails for good.
///
/// `routes` may change at any time (interfaces appearing and disappearing);
/// `on_snapshot` receives progress about four times a second.
pub async fn download(
    spec: Spec,
    cfg: Config,
    mut routes_rx: watch::Receiver<Vec<Route>>,
    mut command: watch::Receiver<Command>,
    on_snapshot: &mut (dyn FnMut(Snapshot) + Send),
) -> Result<Outcome> {
    let primary_url = spec.urls.first().cloned().ok_or_else(|| Error::InvalidUrl("no url".into()))?;
    let opts = RequestOptions {
        user_agent: cfg.user_agent.clone(),
        headers: spec.headers.clone(),
        connect_timeout: cfg.connect_timeout,
        header_timeout: cfg.header_timeout,
    };
    let (mut cmd_closed, mut routes_closed) = (false, false);
    let mut notes: Vec<String> = Vec::new();
    let blank = |phase: Phase, notes: &[String]| Snapshot {
        phase,
        file_name: spec.file_name.clone().or_else(|| name::from_url(&primary_url)).unwrap_or_default(),
        size: None,
        done: 0,
        rate: 0.0,
        eta_secs: None,
        connections: 0,
        routes: Vec::new(),
        map: Vec::new(),
        verdict: Verdict::Network,
        shared_uplink: false,
        resumable: true,
        sources: 1,
        steals: 0,
        flush_ms: 0,
        notes: notes.to_vec(),
    };

    // ---- Probe, retrying over every route until one answers. ----
    let started = Instant::now();
    let mut attempt = 0u32;
    let mut spare: Vec<(Option<BindSpec>, Conn)> = Vec::new();
    let mut head: Option<(Option<BindSpec>, Head)> = None;
    let resuming = journal::exists_for(&spec.dir, primary_url.as_str());
    let primary = loop {
        match *command.borrow() {
            Command::Pause => return Ok(Outcome::Paused),
            Command::Cancel => return Ok(Outcome::Cancelled),
            Command::Run => {}
        }
        let routes = routes_rx.borrow_and_update().clone();
        if routes.is_empty() {
            on_snapshot(blank(Phase::WaitingForNetwork, &notes));
            tokio::select! {
                () = changed(&mut routes_rx, &mut routes_closed) => {}
                () = changed(&mut command, &mut cmd_closed) => {}
                () = tokio::time::sleep(Duration::from_secs(1)), if routes_closed => return Err(Error::NoRoute),
            }
            continue;
        }
        on_snapshot(blank(Phase::Probing, &notes));
        let mut last = None;
        let mut found = None;
        for (i, r) in routes.iter().enumerate() {
            // While the probe is on its way, open the connections the download will
            // start with. On a long path this saves a full handshake before the first byte.
            let warm: Vec<_> = (1..if i == 0 && attempt == 0 { cfg.conns_initial } else { 1 })
                .map(|_| {
                    let (bind, url, opts) = (r.bind.clone(), primary_url.clone(), opts.clone());
                    tokio::spawn(async move { Conn::open(bind.as_ref(), &url, &opts).await })
                })
                .collect();
            // A fresh download lets the answer to the probe double as the first data
            // stream. A resumed one already has the start of the file: it asks for one byte.
            let probed = if resuming {
                probe(r.bind.as_ref(), &primary_url, &opts).await.map(|p| (p, None))
            } else {
                probe_stream(r.bind.as_ref(), &primary_url, &opts).await
            };
            let usable = probed.as_ref().is_ok_and(|(p, _)| p.ranges && p.size.is_some());
            for handle in warm {
                if usable && handle.is_finished() {
                    if let Ok(Ok(conn)) = handle.await {
                        spare.push((r.bind.clone(), conn));
                    }
                } else {
                    handle.abort();
                }
            }
            match probed {
                Ok((p, stream)) => {
                    head = stream.map(|h| (r.bind.clone(), h));
                    found = Some(p);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if let Some(p) = found {
            break p;
        }
        let e = last.unwrap_or(Error::NoRoute);
        if !e.transient() {
            return Err(e);
        }
        if cfg.give_up_after.is_some_and(|d| started.elapsed() > d) {
            return Err(Error::GaveUp(e.to_string()));
        }
        attempt += 1;
        notes = vec![format!("probe failed: {e}")];
        on_snapshot(blank(Phase::Retrying, &notes));
        tokio::select! {
            () = tokio::time::sleep(e.retry_after().unwrap_or_else(|| backoff(attempt))) => {}
            () = changed(&mut command, &mut cmd_closed) => {}
        }
    };
    notes.clear();

    std::fs::create_dir_all(&spec.dir).map_err(Error::Disk)?;
    let (paths, resumed) = plan_paths(&spec, &primary);
    let size = primary.size;
    let whole = !primary.ranges || size.is_none();
    if whole && size.is_some() {
        notes.push("server does not support resuming; using one connection".into());
    }

    // ---- Mirrors: only links that provably serve the same bytes are used. ----
    let first_bind = routes_rx.borrow().first().and_then(|r| r.bind.clone());
    let mut sources = vec![Source::new(primary_url.clone(), &primary)];
    if !whole {
        for url in spec.urls.iter().skip(1) {
            let checked = async {
                let p = probe(first_bind.as_ref(), url, &opts).await?;
                Ok::<_, Error>((same_content(first_bind.as_ref(), &primary, &p, &opts).await?, p))
            }
            .await;
            match checked {
                Ok((true, p)) => sources.push(Source::new(url.clone(), &p)),
                Ok((false, _)) => {
                    notes.push(format!("mirror skipped (different file): {}", url.host_str().unwrap_or("")))
                }
                Err(e) => notes.push(format!("mirror skipped ({e}): {}", url.host_str().unwrap_or(""))),
            }
        }
    }

    let done = resumed.as_ref().map(|j| j.done.clone()).unwrap_or_default();
    let fresh = resumed.is_none();
    if let Some(size) = size {
        let need = size - done.total();
        if let Some(free) = disk::free_space(&spec.dir)
            && free < need
        {
            return Err(Error::DiskFull { need, free });
        }
    }
    let file = disk::open_part(&paths.part, size, fresh).map_err(Error::Disk)?;
    let writer = DiskWriter::start(file, done.clone());
    let pending = match size {
        Some(size) => done.complement(size),
        None => RangeSet::from_spans([(0, UNKNOWN_END)]),
    };
    let mut sched_cfg = cfg.sched.clone();
    if whole {
        // One request for everything, never split.
        sched_cfg.first_piece = u64::MAX;
        sched_cfg.max_piece = u64::MAX;
        sched_cfg.min_steal = u64::MAX / 2;
    }
    let ctx = Arc::new(Ctx {
        opts,
        sched: Mutex::new(Scheduler::new(sched_cfg, pending)),
        writer: writer.clone(),
        sources,
        size,
        final_size: AtomicU64::new(u64::MAX),
        whole,
        spare: Mutex::new(spare),
        cfg: cfg.clone(),
    });

    let journal_of = |done: RangeSet| Journal {
        version: journal::VERSION,
        urls: spec.urls.iter().map(Url::to_string).collect(),
        size,
        etag: primary.etag.clone(),
        last_modified: primary.last_modified.clone(),
        file_name: paths.name.clone(),
        done,
    };
    if !whole && fresh {
        journal::save(&paths.journal, &journal_of(RangeSet::new())).map_err(Error::Disk)?;
    }

    // ---- Supervise. ----
    let mut workers: JoinSet<Exit> = JoinSet::new();
    let mut tasks: HashMap<Id, (String, AbortHandle)> = HashMap::new();
    let mut runs: HashMap<String, RouteRun> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut tick = tokio::time::interval(cfg.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut total_meter = RateMeter::new(Duration::from_millis(500), Instant::now());
    let mut last_done = writer.done_bytes();
    let mut last_progress = Instant::now();
    let mut last_wall = SystemTime::now();
    let mut next_journal = Instant::now() + cfg.journal_interval;
    let mut journal_gap = cfg.journal_interval;
    let mut pressure_ticks = 0u32;
    let mut flush_ms = 0u32;
    let mut refreshes: Vec<Instant> = Vec::new();
    let mut last_error: Option<String> = None;
    let mut routes_dirty = true;

    let stop: Result<Command> = 'run: loop {
        if routes_dirty {
            routes_dirty = false;
            let now = Instant::now();
            let current = routes_rx.borrow_and_update().clone();
            // A route whose addresses changed is a different route as far as sockets go.
            let gone: Vec<String> = runs
                .iter()
                .filter(|(id, run)| !current.iter().any(|r| &r.id == *id && r.bind == run.shared.route.bind))
                .map(|(id, _)| id.clone())
                .collect();
            for id in gone {
                for (rid, handle) in tasks.values() {
                    if rid == &id {
                        handle.abort();
                    }
                }
                runs.remove(&id);
            }
            order = current.iter().map(|r| r.id.clone()).collect();
            for r in current {
                runs.entry(r.id.clone()).or_insert_with(|| RouteRun {
                    shared: Arc::new(RouteShared {
                        route: r,
                        bytes: AtomicU64::new(0),
                        active: AtomicUsize::new(0),
                        target: AtomicUsize::new(1),
                    }),
                    tuner: Tuner::new(
                        if whole { 1 } else { cfg.conns_initial },
                        if whole { 1 } else { cfg.conns_max },
                        now,
                    ),
                    meter: RateMeter::new(Duration::from_millis(250), now),
                    last_bytes: 0,
                    failures: 0,
                    failed_at: None,
                    next_spawn: now,
                    error: None,
                });
            }
        }

        tokio::select! {
            _ = tick.tick() => {}
            Some(joined) = workers.join_next_with_id(), if !workers.is_empty() => {
                let now = Instant::now();
                let (id, exit) = match joined {
                    Ok((id, exit)) => (id, Some(exit)),
                    Err(e) => (e.id(), None),
                };
                let Some((route_id, _)) = tasks.remove(&id) else { continue };
                let Some(run) = runs.get_mut(&route_id) else { continue };
                match exit {
                    None | Some(Exit::Surplus) => {}
                    Some(Exit::NoWork) => run.next_spawn = now + cfg.tick,
                    Some(Exit::Failed { source, generation, error }) => {
                        if error.maybe_expired() && generation < ctx.sources[source].generation.load(Ordering::Acquire) {
                            // It used a link that has been replaced since: just go again.
                            run.next_spawn = now;
                            continue;
                        }
                        let text = error.to_string();
                        last_error = Some(text.clone());
                        run.error = Some(text);
                        if matches!(error, Error::Disk(_) | Error::FileChanged) {
                            break 'run Err(error);
                        }
                        if error.maybe_expired() {
                            // A signed link ran out: ask the original link for a fresh one.
                            refreshes.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                            let src = &ctx.sources[source];
                            let healthy_others = ctx.sources.iter().enumerate().any(|(i, s)| i != source && !s.dead.load(Ordering::Acquire));
                            let fresh_probe = if refreshes.len() < 3 {
                                refreshes.push(now);
                                probe(run.shared.route.bind.as_ref(), &src.requested, &ctx.opts).await
                            } else {
                                Err(Error::LinkExpired)
                            };
                            match fresh_probe {
                                Ok(p) if p.size == size && p.ranges == !whole => {
                                    *src.current.lock().unwrap() = p.url;
                                    src.generation.fetch_add(1, Ordering::AcqRel);
                                    run.next_spawn = now;
                                    continue;
                                }
                                Ok(_) => break 'run Err(Error::FileChanged),
                                Err(e) if e.transient() => {}
                                Err(_) if healthy_others => {
                                    src.dead.store(true, Ordering::Release);
                                    notes.push("a mirror stopped working and was dropped".into());
                                }
                                Err(_) => break 'run Err(if refreshes.len() >= 3 { Error::LinkExpired } else { error }),
                            }
                        } else if !error.transient() {
                            break 'run Err(error);
                        }
                        if error.refusal() {
                            run.tuner.refused(now, run.shared.active.load(Ordering::Acquire) + 1);
                        }
                        // Several connections failing in the same instant are one event, and a
                        // route that delivered data since its last failure starts over at one.
                        let delivered = run.shared.bytes.load(Ordering::Relaxed);
                        match run.failed_at {
                            Some((at, _)) if now.duration_since(at) < Duration::from_millis(300) => {}
                            Some((_, bytes)) if delivered > bytes => run.failures = 1,
                            _ => run.failures += 1,
                        }
                        run.failures = run.failures.max(1);
                        run.failed_at = Some((now, delivered));
                        let wait = if error.refusal() { refusal_backoff(run.failures) } else { backoff(run.failures) };
                        run.next_spawn = now + error.retry_after().map_or(wait, |r| r.max(Duration::from_secs(1)));
                        if matches!(error, Error::Status { code: 429, .. }) {
                            ctx.sched.lock().unwrap().prefer_few_requests();
                            if !notes.iter().any(|n| n.contains("limits requests")) {
                                notes.push("server limits requests; using fewer, longer ones".into());
                            }
                        }
                    }
                }
                continue;
            }
            () = changed(&mut command, &mut cmd_closed) => {
                let c = *command.borrow();
                if c != Command::Run {
                    break 'run Ok(c);
                }
                continue;
            }
            () = changed(&mut routes_rx, &mut routes_closed) => {
                routes_dirty = true;
                continue;
            }
        }

        // ---- Housekeeping, once per tick. ----
        let now = Instant::now();
        let wall = SystemTime::now();
        let slept = wall.duration_since(last_wall).is_ok_and(|d| d > cfg.tick + Duration::from_secs(5));
        last_wall = wall;
        if slept {
            // The machine was suspended: every socket is dead whatever TCP still believes.
            for (_, handle) in tasks.values() {
                handle.abort();
            }
            for run in runs.values_mut() {
                run.failures = 0;
                run.next_spawn = now;
            }
            last_progress = now;
            continue;
        }

        let done_bytes = writer.done_bytes();
        if done_bytes > last_done {
            last_progress = now;
        }
        total_meter.add(done_bytes.saturating_sub(last_done), now);
        last_done = done_bytes;

        // Disk falling behind: fewer parallel streams make the writes more sequential.
        pressure_ticks = if writer.pressure() > 0.9 { pressure_ticks + 1 } else { 0 };
        let disk_slow = pressure_ticks >= 4;
        if disk_slow {
            pressure_ticks = 0;
        }

        let parallel: usize = runs.values().map(|r| r.tuner.target()).sum();
        let (unassigned, has_work, steals, in_flight) = {
            let mut s = ctx.sched.lock().unwrap();
            s.set_parallel(parallel.min(cfg.conns_total));
            (s.unassigned(), s.has_work(), s.steals(), s.in_flight())
        };
        // Close to the end a new connection would still be in its handshake when the
        // others finish; only connections that are already open take over tails then.
        let nearly_done = unassigned == 0 && {
            let rate = total_meter.rate(now);
            rate > 0.0 && in_flight as f64 / rate < 1.5
        };
        let mut total_active: usize = runs.values().map(|r| r.shared.active.load(Ordering::Acquire)).sum();
        let mut budget = if unassigned > 0 {
            unassigned.div_ceil(ctx.cfg.sched.min_piece.max(1)).min(1024) as usize
        } else {
            if has_work && !nearly_done { 4 } else { 0 }
        };
        let conns_total = if whole { 1 } else { cfg.conns_total };
        if let Some((bind, stream)) = head.take()
            && let Some((id, run)) = runs.iter().find(|(_, r)| r.shared.route.bind == bind)
        {
            // The answer to the probe is the file arriving from byte 0. Give it the
            // first lease before any other connection can take those bytes.
            let lease = ctx.sched.lock().unwrap().acquire(0.0);
            match lease {
                Some(lease) if lease.start == 0 => {
                    let guard = ActiveGuard::new(ctx.clone(), run.shared.clone(), 0);
                    let handle = workers.spawn(worker(guard, Some((lease, stream))));
                    tasks.insert(handle.id(), (id.clone(), handle));
                    total_active += 1;
                    budget = budget.saturating_sub(1);
                }
                // Resuming somewhere else in the file: that stream is no use.
                Some(lease) => ctx.sched.lock().unwrap().release(lease.id),
                None => {}
            }
        }
        for id in &order {
            let Some(run) = runs.get_mut(id) else { continue };
            let bytes = run.shared.bytes.load(Ordering::Relaxed);
            if bytes > run.last_bytes {
                run.failures = 0;
                run.error = None;
            }
            run.meter.add(bytes - run.last_bytes, now);
            run.last_bytes = bytes;
            let active = run.shared.active.load(Ordering::Acquire);
            if disk_slow {
                run.tuner.disk_slow(now);
            }
            run.tuner.tick(now, run.meter.rate(now), active, budget == 0);
            let target = run.tuner.target();
            run.shared.target.store(target, Ordering::Release);
            if now < run.next_spawn {
                continue;
            }
            // While a route is failing, one connection keeps testing it.
            let want = if run.failures > 0 { 1 } else { target };
            let mut active = active;
            while active < want && total_active < conns_total && budget > 0 {
                let source = ctx
                    .sources
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.dead.load(Ordering::Acquire))
                    .min_by_key(|(_, s)| s.active.load(Ordering::Acquire))
                    .map_or(0, |(i, _)| i);
                let guard = ActiveGuard::new(ctx.clone(), run.shared.clone(), source);
                let handle = workers.spawn(worker(guard, None));
                tasks.insert(handle.id(), (id.clone(), handle));
                active += 1;
                total_active += 1;
                budget -= 1;
            }
        }

        // ---- Finished? ----
        let final_size =
            size.or_else(|| Some(ctx.final_size.load(Ordering::Acquire)).filter(|s| *s != u64::MAX));
        if ctx.sched.lock().unwrap().is_idle()
            && let Some(total) = final_size
        {
            // Every lease is closed. Wait for the disk thread to catch up before judging.
            let missing = match writer.sync().await {
                Ok(durable) => durable.complement(total),
                Err(e) => break 'run Err(Error::Disk(e)),
            };
            if missing.is_empty() {
                break 'run Ok(Command::Run);
            }
            let mut s = ctx.sched.lock().unwrap();
            for &(a, b) in missing.spans() {
                s.requeue(a, b);
            }
        }

        if !whole && now >= next_journal {
            let t = Instant::now();
            match writer.sync().await {
                Ok(durable) => journal::save(&paths.journal, &journal_of(durable)).map_err(Error::Disk)?,
                Err(e) => break 'run Err(Error::Disk(e)),
            }
            // A slow flush (spinning disk) must not eat the disk's time: back off.
            flush_ms = t.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            journal_gap = cfg.journal_interval.max(t.elapsed() * 5).min(Duration::from_secs(30));
            next_journal = Instant::now() + journal_gap;
        }

        if let Some(limit) = cfg.give_up_after
            && now.duration_since(last_progress) > limit
        {
            break 'run Err(Error::GaveUp(last_error.clone().unwrap_or_else(|| "no data received".into())));
        }

        // ---- Report. ----
        let rate = total_meter.rate(now);
        let any_limit = cfg.limiter.rate() > 0 || runs.values().any(|r| r.shared.route.limiter.rate() > 0);
        let limit_sum = {
            let per: u64 = runs.values().map(|r| r.shared.route.limiter.rate()).sum();
            match (cfg.limiter.rate(), per) {
                (0, p) => p,
                (g, 0) => g,
                (g, p) => g.min(p),
            }
        };
        let verdict = if whole {
            Verdict::ServerSingleStream
        } else if runs.values().any(|r| r.tuner.disk_limited(now)) {
            Verdict::Disk
        } else if any_limit && limit_sum > 0 && rate >= limit_sum as f64 * 0.85 {
            Verdict::UserLimit
        } else if runs.values().any(|r| r.tuner.refusing()) {
            Verdict::ServerRefusing
        } else if runs.values().any(|r| r.tuner.per_connection_capped()) {
            Verdict::ServerPerConnection
        } else {
            Verdict::Network
        };
        let mut uplinks: Vec<&str> = runs
            .values()
            .filter(|r| r.shared.active.load(Ordering::Acquire) > 0)
            .filter_map(|r| r.shared.route.uplink.as_deref())
            .collect();
        let before = uplinks.len();
        uplinks.sort_unstable();
        uplinks.dedup();
        let shared_uplink = uplinks.len() < before;
        let phase = if runs.is_empty() {
            Phase::WaitingForNetwork
        } else if total_active == 0 && runs.values().all(|r| r.failures > 0) {
            Phase::Retrying
        } else {
            Phase::Downloading
        };
        let mut route_snaps = Vec::with_capacity(order.len());
        for id in &order {
            let Some(r) = runs.get_mut(id) else { continue };
            let connections = r.shared.active.load(Ordering::Acquire);
            route_snaps.push(RouteSnapshot {
                id: r.shared.route.id.clone(),
                label: r.shared.route.label.clone(),
                bytes: r.last_bytes,
                rate: r.meter.rate(now),
                connections,
                target: r.tuner.target(),
                health: if r.failures > 0 {
                    RouteHealth::Retrying
                } else if connections > 0 {
                    RouteHealth::Active
                } else {
                    RouteHealth::Idle
                },
                error: r.error.clone(),
            });
        }
        on_snapshot(Snapshot {
            phase,
            file_name: paths.name.clone(),
            size: final_size,
            done: done_bytes,
            rate,
            eta_secs: final_size.filter(|_| rate > 1.0).map(|s| s.saturating_sub(done_bytes) as f64 / rate),
            connections: total_active,
            routes: route_snaps,
            map: size.map(|s| writer.done().density(s, cfg.map_cells)).unwrap_or_default(),
            verdict,
            shared_uplink,
            resumable: !whole,
            sources: ctx.sources.iter().filter(|s| !s.dead.load(Ordering::Acquire)).count(),
            steals,
            flush_ms,
            notes: notes.clone(),
        });
    };

    // ---- Wind down: stop every connection before touching the file. ----
    workers.shutdown().await;
    drop(tasks);
    let _ = journal_gap;

    let finish = |snapshot_phase: Phase,
                  on_snapshot: &mut (dyn FnMut(Snapshot) + Send),
                  done: u64,
                  total: Option<u64>| {
        let mut s = blank(snapshot_phase, &notes);
        s.file_name = paths.name.clone();
        s.size = total;
        s.done = done;
        s.resumable = !whole;
        on_snapshot(s);
    };

    match stop {
        Ok(Command::Run) => {
            let total = size.unwrap_or_else(|| ctx.final_size.load(Ordering::Acquire));
            writer.sync().await.map_err(Error::Disk)?;
            writer.close().await;
            let expected = spec
                .checksum
                .clone()
                .or_else(|| primary.sha256.clone().map(|hex| Checksum { algorithm: Algorithm::Sha256, hex }));
            if let Some(expected) = expected {
                finish(Phase::Verifying, on_snapshot, total, Some(total));
                let part = paths.part.clone();
                let alg = expected.algorithm;
                let actual = tokio::task::spawn_blocking(move || hash::file_digest(&part, alg, |_| {}))
                    .await
                    .map_err(|e| Error::Disk(std::io::Error::other(e)))?
                    .map_err(Error::Disk)?;
                if actual != expected.hex {
                    // Nothing says which bytes are wrong, so nothing can be kept.
                    journal::remove(&paths.journal);
                    let _ = std::fs::remove_file(&paths.part);
                    return Err(Error::Integrity { expected: expected.hex, actual });
                }
            }
            // The name was free when the download started; make sure it still is.
            let name = name::unique(&spec.dir, &paths.name, Path::exists);
            let dest = spec.dir.join(name);
            std::fs::rename(&paths.part, &dest).map_err(Error::Disk)?;
            journal::remove(&paths.journal);
            Ok(Outcome::Completed { path: dest, size: total })
        }
        Ok(Command::Cancel) => {
            writer.close().await;
            journal::remove(&paths.journal);
            let _ = std::fs::remove_file(&paths.part);
            Ok(Outcome::Cancelled)
        }
        Ok(Command::Pause) | Err(_) => {
            // Keep everything that is safely on disk so the next run continues from here.
            if !whole && let Ok(durable) = writer.sync().await {
                let _ = journal::save(&paths.journal, &journal_of(durable));
            }
            writer.close().await;
            if whole {
                let _ = std::fs::remove_file(&paths.part);
            }
            stop.map(|_| Outcome::Paused)
        }
    }
}
