//! What a download is given and what it reports.

use super::*;

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
            journal_interval: Duration::from_millis(500),
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
