//! The download list: queueing, running a few at a time, remembering across restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, watch};
use url::Url;

use crate::download::{Command, Config, Outcome, Route, Snapshot, Spec, download};
use crate::hash::Checksum;
use crate::limit::Limiter;
use crate::routes::{RouteBook, Selection, watch_routes};
use crate::secret;

/// Which networks downloads may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", content = "names", rename_all = "snake_case")]
pub enum Networks {
    /// Every network with its own way out, unless a VPN is up.
    #[default]
    Auto,
    /// Whatever the operating system routes through.
    System,
    /// Only the named interfaces.
    Only(Vec<String>),
}

impl From<&Networks> for Selection {
    fn from(n: &Networks) -> Self {
        match n {
            Networks::Auto => Selection::Auto,
            Networks::System => Selection::System,
            Networks::Only(names) => Selection::Only(names.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// Downloads running at the same time.
    pub max_concurrent: usize,
    /// Most connections one network may open for one download.
    pub connections_per_network: usize,
    /// Bytes per second for everything together; 0 is unlimited.
    pub speed_limit: u64,
    /// Bytes per second per network, by interface name; 0 or absent is unlimited.
    pub network_limits: HashMap<String, u64>,
    pub networks: Networks,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: PathBuf::from("."),
            max_concurrent: 3,
            connections_per_network: 32,
            speed_limit: 0,
            network_limits: HashMap::new(),
            networks: Networks::Auto,
        }
    }
}

/// What the user (or the browser extension) asks for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewDownload {
    /// The link, then optional mirrors of the same file.
    pub urls: Vec<String>,
    pub dir: Option<PathBuf>,
    pub file_name: Option<String>,
    pub headers: Vec<(String, String)>,
    pub checksum: Option<String>,
    /// Add it paused instead of starting when a slot is free.
    pub paused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
}

/// One row of the download list, as shown to the user.
#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub id: u64,
    pub urls: Vec<String>,
    pub dir: PathBuf,
    pub file_name: String,
    pub status: Status,
    pub error: Option<String>,
    /// Where the finished file is.
    pub path: Option<PathBuf>,
    pub size: Option<u64>,
    pub done: u64,
    pub added_at: u64,
    pub finished_at: Option<u64>,
    /// Live details while running.
    pub snapshot: Option<Snapshot>,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// An item was added or changed (status or progress).
    Item(Box<ItemView>),
    Removed(u64),
    Settings(Box<Settings>),
}

/// What is written to disk for each item. Cookies and credentials are sealed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: u64,
    urls: Vec<String>,
    dir: PathBuf,
    file_name: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    sealed_headers: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    status: Status,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    done: u64,
    added_at: u64,
    #[serde(default)]
    finished_at: Option<u64>,
    #[serde(default)]
    shown_name: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    items: Vec<Record>,
}

struct Item {
    rec: Record,
    /// Headers that are never written to disk in the clear.
    secret_headers: Vec<(String, String)>,
    snapshot: Option<Snapshot>,
    control: Option<watch::Sender<Command>>,
    /// Remove the item (and optionally its file) once its task has stopped.
    remove_when_stopped: bool,
    /// Resume arrived while a pause was still stopping the download.
    resume_when_stopped: bool,
}

impl Item {
    fn view(&self) -> ItemView {
        let r = &self.rec;
        ItemView {
            id: r.id,
            urls: r.urls.clone(),
            dir: r.dir.clone(),
            file_name: r.shown_name.clone(),
            status: r.status,
            error: r.error.clone(),
            path: r.path.clone(),
            size: r.size,
            done: r.done,
            added_at: r.added_at,
            finished_at: r.finished_at,
            snapshot: self.snapshot.clone(),
        }
    }
}

struct State {
    settings: Settings,
    next_id: u64,
    items: Vec<Item>,
}

struct Inner {
    state: Mutex<State>,
    file: PathBuf,
    /// Held while the list is written, so two saves never share the temporary file.
    saving: Mutex<()>,
    book: Arc<RouteBook>,
    limiter: Arc<Limiter>,
    selection: watch::Sender<Selection>,
    routes: watch::Receiver<Vec<Route>>,
    events: broadcast::Sender<Event>,
    wake: Notify,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn is_secret(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "cookie" | "authorization" | "proxy-authorization")
}

/// The list of downloads. Cheap to clone; all clones share one list.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

impl Manager {
    /// Opens the list stored in `state_dir` (created if missing) and continues
    /// whatever was running when the program last stopped. Needs a Tokio runtime.
    pub fn open(state_dir: &Path, default_download_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let file = state_dir.join("downloads.json");
        let on_disk = std::fs::read(&file).ok();
        let parsed: Option<Saved> = on_disk.as_deref().and_then(|b| serde_json::from_slice(b).ok());
        if parsed.is_none() && on_disk.is_some_and(|b| !b.is_empty()) {
            // An unreadable list is set aside, not overwritten: it may still be repaired by hand.
            let _ = std::fs::rename(&file, file.with_extension("json.unreadable"));
        }
        let mut saved: Saved = parsed.unwrap_or_else(|| Saved {
            settings: Settings { download_dir: default_download_dir.into(), ..Settings::default() },
            ..Saved::default()
        });
        saved.settings.max_concurrent = saved.settings.max_concurrent.clamp(1, 16);
        saved.settings.connections_per_network = saved.settings.connections_per_network.clamp(1, 64);
        let items = saved
            .items
            .into_iter()
            .map(|mut rec| {
                // Whatever was running when the program stopped carries on by itself.
                if rec.status == Status::Running {
                    rec.status = Status::Queued;
                }
                let secret_headers = rec
                    .sealed_headers
                    .as_deref()
                    .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
                    .and_then(|sealed| secret::open(&sealed))
                    .and_then(|plain| serde_json::from_slice(&plain).ok())
                    .unwrap_or_default();
                Item {
                    rec,
                    secret_headers,
                    snapshot: None,
                    control: None,
                    remove_when_stopped: false,
                    resume_when_stopped: false,
                }
            })
            .collect();
        let book = Arc::new(RouteBook::default());
        let limiter = Arc::new(Limiter::new(saved.settings.speed_limit));
        for (id, rate) in &saved.settings.network_limits {
            book.limiter(id).set_rate(*rate);
        }
        let (selection, selection_rx) = watch::channel(Selection::from(&saved.settings.networks));
        let routes = watch_routes(book.clone(), selection_rx);
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            state: Mutex::new(State { settings: saved.settings, next_id: saved.next_id.max(1), items }),
            file,
            saving: Mutex::new(()),
            book,
            limiter,
            selection,
            routes,
            events,
            wake: Notify::new(),
        });
        let pump = inner.clone();
        tokio::spawn(async move {
            loop {
                Inner::start_queued(&pump);
                pump.wake.notified().await;
            }
        });
        Ok(Self { inner })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn list(&self) -> Vec<ItemView> {
        self.inner.state.lock().unwrap().items.iter().map(Item::view).collect()
    }

    pub fn settings(&self) -> Settings {
        self.inner.state.lock().unwrap().settings.clone()
    }

    /// The routes downloads are using right now.
    pub fn routes(&self) -> Vec<Route> {
        self.inner.routes.borrow().clone()
    }

    pub fn set_settings(&self, mut settings: Settings) {
        settings.max_concurrent = settings.max_concurrent.clamp(1, 16);
        settings.connections_per_network = settings.connections_per_network.clamp(1, 64);
        self.inner.limiter.set_rate(settings.speed_limit);
        {
            let mut st = self.inner.state.lock().unwrap();
            // A limit that was removed has to be lifted, not just forgotten.
            for id in st.settings.network_limits.keys() {
                if !settings.network_limits.contains_key(id) {
                    self.inner.book.limiter(id).set_rate(0);
                }
            }
            for (id, rate) in &settings.network_limits {
                self.inner.book.limiter(id).set_rate(*rate);
            }
            st.settings = settings.clone();
        }
        let _ = self.inner.selection.send(Selection::from(&settings.networks));
        self.inner.save();
        let _ = self.inner.events.send(Event::Settings(Box::new(settings)));
        self.inner.wake.notify_one();
    }

    /// Adds a download. Only `http` and `https` links are accepted.
    pub fn add(&self, new: NewDownload) -> Result<u64, String> {
        if new.urls.is_empty() {
            return Err("no link given".into());
        }
        for u in &new.urls {
            let url = Url::parse(u.trim()).map_err(|e| format!("{u}: {e}"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(format!("{u}: only http and https links are supported"));
            }
        }
        if let Some(c) = &new.checksum
            && !c.trim().is_empty()
            && Checksum::parse(c).is_none()
        {
            return Err("the checksum is not a valid sha256, sha1 or md5 value".into());
        }
        let (public, secret_headers): (Vec<_>, Vec<_>) =
            new.headers.into_iter().partition(|(k, _)| !is_secret(k));
        let sealed_headers = (!secret_headers.is_empty())
            .then(|| serde_json::to_vec(&secret_headers).ok())
            .flatten()
            .and_then(|plain| secret::seal(&plain))
            .map(|sealed| base64::engine::general_purpose::STANDARD.encode(sealed));
        let view = {
            let mut st = self.inner.state.lock().unwrap();
            let id = st.next_id;
            st.next_id += 1;
            let urls: Vec<String> = new.urls.iter().map(|u| u.trim().to_string()).collect();
            let shown_name = new
                .file_name
                .clone()
                .or_else(|| Url::parse(&urls[0]).ok().and_then(|u| crate::name::from_url(&u)))
                .unwrap_or_else(|| urls[0].clone());
            let rec = Record {
                id,
                urls,
                dir: new.dir.unwrap_or_else(|| st.settings.download_dir.clone()),
                file_name: new.file_name.filter(|n| !n.trim().is_empty()),
                headers: public,
                sealed_headers,
                checksum: new.checksum.filter(|c| !c.trim().is_empty()),
                status: if new.paused { Status::Paused } else { Status::Queued },
                error: None,
                path: None,
                size: None,
                done: 0,
                added_at: now(),
                finished_at: None,
                shown_name,
            };
            let item = Item {
                rec,
                secret_headers,
                snapshot: None,
                control: None,
                remove_when_stopped: false,
                resume_when_stopped: false,
            };
            let view = item.view();
            st.items.push(item);
            view
        };
        let id = view.id;
        self.inner.save();
        let _ = self.inner.events.send(Event::Item(Box::new(view)));
        self.inner.wake.notify_one();
        Ok(id)
    }

    /// Pauses a queued or running download. Progress is kept.
    pub fn pause(&self, id: u64) {
        self.inner.change(id, |item| match item.rec.status {
            Status::Running => {
                item.resume_when_stopped = false;
                if let Some(c) = &item.control {
                    let _ = c.send(Command::Pause);
                }
            }
            Status::Queued => item.rec.status = Status::Paused,
            _ => {}
        });
    }

    /// Queues a paused or failed download again.
    pub fn resume(&self, id: u64) {
        self.inner.change(id, |item| {
            if matches!(item.rec.status, Status::Paused | Status::Failed) {
                item.rec.status = Status::Queued;
                item.rec.error = None;
            } else if item.rec.status == Status::Running
                && item.control.as_ref().is_some_and(|c| *c.borrow() == Command::Pause)
            {
                // The pause is still stopping the download: queue it again once it has.
                item.resume_when_stopped = true;
            }
        });
    }

    /// Takes a download off the list. A partial file is always deleted; the
    /// finished file only when `delete_file` is set.
    pub fn remove(&self, id: u64, delete_file: bool) {
        let mut removed = false;
        {
            let mut st = self.inner.state.lock().unwrap();
            let Some(pos) = st.items.iter().position(|i| i.rec.id == id) else { return };
            let item = &mut st.items[pos];
            if let Some(c) = &item.control {
                // Running: the task deletes its partial file and then drops the row.
                item.remove_when_stopped = true;
                let _ = c.send(Command::Cancel);
            } else {
                let item = st.items.remove(pos);
                removed = true;
                // Only a file in the download's own folder is ever deleted, whatever
                // the saved list claims.
                if delete_file
                    && let Some(p) = &item.rec.path
                    && p.parent() == Some(item.rec.dir.as_path())
                {
                    let _ = std::fs::remove_file(p);
                }
                // A paused or failed download leaves its partial file behind.
                let name =
                    item.snapshot.as_ref().map_or(item.rec.shown_name.as_str(), |s| s.file_name.as_str());
                if item.rec.status != Status::Completed && !name.is_empty() {
                    let part = crate::name::part_path(&item.rec.dir, &crate::name::sanitize(name));
                    let _ = std::fs::remove_file(crate::journal::journal_path(&part));
                    let _ = std::fs::remove_file(part);
                }
            }
        }
        if removed {
            self.inner.save();
            let _ = self.inner.events.send(Event::Removed(id));
        }
    }

    /// Moves a download to `index` in the list. Queued downloads start in list order.
    pub fn move_to(&self, id: u64, index: usize) {
        {
            let mut st = self.inner.state.lock().unwrap();
            let Some(pos) = st.items.iter().position(|i| i.rec.id == id) else { return };
            let item = st.items.remove(pos);
            let index = index.min(st.items.len());
            st.items.insert(index, item);
        }
        self.inner.save();
        self.inner.wake.notify_one();
    }

    /// Forgets every finished download (the files stay).
    pub fn clear_completed(&self) {
        let gone: Vec<u64> = {
            let mut st = self.inner.state.lock().unwrap();
            let ids =
                st.items.iter().filter(|i| i.rec.status == Status::Completed).map(|i| i.rec.id).collect();
            st.items.retain(|i| i.rec.status != Status::Completed);
            ids
        };
        self.inner.save();
        for id in gone {
            let _ = self.inner.events.send(Event::Removed(id));
        }
    }
}

impl Inner {
    fn save(&self) {
        // The state is read under the same lock that orders the writes, so the
        // file on disk is never older than a save that finished earlier.
        let _writing = self.saving.lock().unwrap();
        let saved = {
            let st = self.state.lock().unwrap();
            Saved {
                settings: st.settings.clone(),
                next_id: st.next_id,
                items: st.items.iter().map(|i| i.rec.clone()).collect(),
            }
        };
        let Ok(bytes) = serde_json::to_vec_pretty(&saved) else { return };
        let tmp = self.file.with_extension("json.tmp");
        // Written in full and flushed before it replaces the old list: a crash or a
        // power cut leaves either the old list or the new one, never half of one.
        let written = (|| {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()
        })();
        if written.is_ok() {
            // If even the patient move fails, the next change saves again; the list in
            // memory stays the truth meanwhile.
            let _ = crate::disk::replace_file(&tmp, &self.file);
        }
    }

    fn change(self: &Arc<Self>, id: u64, f: impl FnOnce(&mut Item)) {
        let view = {
            let mut st = self.state.lock().unwrap();
            let Some(item) = st.items.iter_mut().find(|i| i.rec.id == id) else { return };
            f(item);
            item.view()
        };
        self.save();
        let _ = self.events.send(Event::Item(Box::new(view)));
        self.wake.notify_one();
    }

    /// Starts queued downloads, in list order, while slots are free.
    fn start_queued(self: &Arc<Self>) {
        loop {
            let (spec, cfg, id, control_rx) = {
                let mut st = self.state.lock().unwrap();
                let running = st.items.iter().filter(|i| i.rec.status == Status::Running).count();
                if running >= st.settings.max_concurrent {
                    return;
                }
                let conns = st.settings.connections_per_network;
                let Some(item) = st.items.iter_mut().find(|i| i.rec.status == Status::Queued) else { return };
                let urls: Vec<Url> = item.rec.urls.iter().filter_map(|u| Url::parse(u).ok()).collect();
                let mut headers = item.rec.headers.clone();
                headers.extend(item.secret_headers.iter().cloned());
                let spec = Spec {
                    urls,
                    dir: item.rec.dir.clone(),
                    file_name: item.rec.file_name.clone(),
                    headers,
                    checksum: item.rec.checksum.as_deref().and_then(Checksum::parse),
                };
                let cfg = Config {
                    conns_max: conns.max(1),
                    conns_initial: Config::default().conns_initial.min(conns.max(1)),
                    limiter: self.limiter.clone(),
                    ..Config::default()
                };
                let (tx, rx) = watch::channel(Command::Run);
                item.control = Some(tx);
                item.rec.status = Status::Running;
                item.rec.error = None;
                (spec, cfg, item.rec.id, rx)
            };
            self.save();
            let me = self.clone();
            let routes = self.routes.clone();
            tokio::spawn(async move {
                let progress = me.clone();
                let mut on_snapshot = move |snap: Snapshot| {
                    let view = {
                        let mut st = progress.state.lock().unwrap();
                        let Some(item) = st.items.iter_mut().find(|i| i.rec.id == id) else { return };
                        item.rec.done = snap.done;
                        item.rec.size = snap.size.or(item.rec.size);
                        if !snap.file_name.is_empty() {
                            item.rec.shown_name.clone_from(&snap.file_name);
                        }
                        item.snapshot = Some(snap);
                        item.view()
                    };
                    let _ = progress.events.send(Event::Item(Box::new(view)));
                };
                let result = download(spec, cfg, routes, control_rx, &mut on_snapshot).await;
                let mut dropped = false;
                let view = {
                    let mut st = me.state.lock().unwrap();
                    let Some(pos) = st.items.iter().position(|i| i.rec.id == id) else { return };
                    let item = &mut st.items[pos];
                    item.control = None;
                    item.snapshot = None;
                    match result {
                        Ok(Outcome::Completed { path, size }) => {
                            item.rec.status = Status::Completed;
                            item.rec.shown_name = path
                                .file_name()
                                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                            item.rec.path = Some(path);
                            item.rec.size = Some(size);
                            item.rec.done = size;
                            item.rec.finished_at = Some(now());
                        }
                        Ok(Outcome::Paused) if item.resume_when_stopped => item.rec.status = Status::Queued,
                        Ok(Outcome::Paused) => item.rec.status = Status::Paused,
                        Ok(Outcome::Cancelled) => item.rec.status = Status::Paused,
                        Err(e) => {
                            item.rec.status = Status::Failed;
                            item.rec.error = Some(e.to_string());
                        }
                    }
                    item.resume_when_stopped = false;
                    if item.remove_when_stopped {
                        st.items.remove(pos);
                        dropped = true;
                        None
                    } else {
                        Some(st.items[pos].view())
                    }
                };
                me.save();
                let _ = match (view, dropped) {
                    (Some(v), _) => me.events.send(Event::Item(Box::new(v))),
                    _ => me.events.send(Event::Removed(id)),
                };
                me.wake.notify_one();
            });
        }
    }
}
