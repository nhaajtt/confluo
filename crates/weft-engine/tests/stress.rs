//! Harder conditions than the end-to-end tests: outages, refusals over time,
//! many downloads at once, pause/resume storms and files past 4 GiB.
//! Every test that completes compares the file with what the server holds.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use url::Url;
use weft_engine::limit::Limiter;
use weft_engine::manager::{Manager, Networks, NewDownload, Settings, Status};
use weft_engine::{Command, Config, Outcome, Route, Snapshot, Spec, download};
use weft_testsrv::{Behavior, Server, content, fill};

const MIB: u64 = 1 << 20;

fn spec(dir: &Path, url: &str) -> Spec {
    Spec {
        urls: vec![Url::parse(url).unwrap()],
        dir: dir.to_path_buf(),
        file_name: None,
        headers: Vec::new(),
        checksum: None,
    }
}

async fn run(
    spec: Spec,
    cfg: Config,
    routes: Vec<Route>,
) -> (weft_engine::Result<Outcome>, Vec<(Instant, Snapshot)>) {
    let (_rtx, rrx) = watch::channel(routes);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut snaps = Vec::new();
    let result = download(spec, cfg, rrx, crx, &mut |s| snaps.push((Instant::now(), s))).await;
    (result, snaps)
}

/// Compares a file with the server's content without holding either in memory.
fn assert_same(path: &Path, seed: u64, size: u64) {
    assert_eq!(std::fs::metadata(path).unwrap().len(), size);
    let mut file = std::fs::File::open(path).unwrap();
    let mut got = vec![0u8; 4 << 20];
    let mut want = vec![0u8; 4 << 20];
    let mut at = 0u64;
    while at < size {
        let n = (size - at).min(got.len() as u64) as usize;
        file.read_exact(&mut got[..n]).unwrap();
        fill(seed, at, &mut want[..n]);
        assert!(got[..n] == want[..n], "content differs somewhere in {at}..{}", at + n as u64);
        at += n as u64;
    }
}

fn settings(dir: &Path, max: usize) -> Settings {
    Settings {
        download_dir: dir.into(),
        max_concurrent: max,
        networks: Networks::System,
        ..Settings::default()
    }
}

async fn wait_for(m: &Manager, secs: u64, what: &str, pred: impl Fn(&Manager) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !pred(m) {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}: {:#?}", m.list());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A server that takes four connections refuses the other four of the first eight
/// in the same instant, which can leave fewer than four running. The engine has to
/// find its way back to four within a second or two, not a quarter of a minute,
/// and must not keep hammering a server that keeps saying no.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_connections_are_won_back_quickly_and_politely() {
    let size = 24 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 41,
        max_conns: Some(4),
        rate_per_conn: Some(MIB),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let (result, snaps) = run(
        spec(dir.path(), &srv.url()),
        Config { give_up_after: Some(Duration::from_secs(30)), ..Config::default() },
        vec![Route::system()],
    )
    .await;
    let Ok(Outcome::Completed { path, .. }) = result else { panic!("{result:?}") };
    assert_same(&path, 41, size);
    let took = t.elapsed().as_secs_f64();
    // Four connections at 1 MiB/s need 6 s; three need 8 s.
    assert!(took < 7.3, "took {took:.1} s: the fourth connection came back too late");
    let full_by =
        snaps.iter().find(|(_, s)| s.connections >= 4 && s.done > 0).map(|(at, _)| at.duration_since(t));
    assert!(full_by.is_some_and(|d| d < Duration::from_secs(3)), "four connections only after {full_by:?}");
    // 1 probe stream, a few ranges, and the refused tests: 1, 2, 4 s apart.
    let requests = srv.stats().requests.load(Ordering::Relaxed);
    assert!(requests < 40, "{requests} requests against a server that allows four connections");
}

/// The server disappears completely for a while (every connection cut, nothing
/// listening) and comes back. The download waits, then carries on where it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_outage_is_ridden_out() {
    let size = 12 * MIB;
    let behavior = || Behavior { size, seed: 43, rate_total: Some(4 * MIB), ..Default::default() };
    let srv = Server::local(behavior()).await;
    let (addr, url) = (srv.addr, srv.url());
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        stall_timeout: Duration::from_secs(2),
        give_up_after: Some(Duration::from_secs(30)),
        ..Config::default()
    };
    let task = tokio::spawn({
        let spec = spec(dir.path(), &url);
        async move { run(spec, cfg, vec![Route::system()]).await }
    });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let sent_before = srv.stats().body_bytes.load(Ordering::Relaxed);
    assert!(sent_before > MIB, "nothing was downloaded before the outage");
    drop(srv);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let back = Server::start(addr, behavior()).await.expect("the port is free again");
    let (result, snaps) = task.await.unwrap();
    let Ok(Outcome::Completed { path, .. }) = result else { panic!("{result:?}") };
    assert_same(&path, 43, size);
    assert!(
        snaps.iter().any(|(_, s)| s.routes.iter().any(|r| r.error.is_some())),
        "the outage went unnoticed"
    );
    let again = back.stats().body_bytes.load(Ordering::Relaxed);
    assert!(sent_before + again < size + 3 * MIB, "refetched too much: {sent_before} + {again} for {size}");
}

/// A speed limit set by the user must not make the engine open more and more
/// connections: they cannot add speed, they only cut the file into slow pieces.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_speed_limit_does_not_multiply_connections() {
    let size = 6 * MIB;
    let srv = Server::local(Behavior { size, seed: 47, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let route = Route {
        id: "capped".into(),
        label: "capped".into(),
        bind: None,
        limiter: Arc::new(Limiter::new(MIB)),
        uplink: None,
    };
    let (result, snaps) = run(spec(dir.path(), &srv.url()), Config::default(), vec![route]).await;
    let Ok(Outcome::Completed { path, .. }) = result else { panic!("{result:?}") };
    assert_same(&path, 47, size);
    let most = snaps.iter().flat_map(|(_, s)| s.routes.iter().map(|r| r.target)).max().unwrap();
    assert!(most <= Config::default().conns_initial, "grew to {most} connections under a 1 MiB/s limit");
}

/// Eighteen downloads through one list, six at a time, from a server that keeps
/// cutting connections. All arrive, all are intact, nothing is left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_downloads_at_once_all_arrive_intact() {
    let size = 3 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 53,
        rate_total: Some(24 * MIB),
        drop_after: Some(700 * 1024),
        drop_count: 40,
        ..Default::default()
    })
    .await;
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let m = Manager::open(state.path(), files.path()).unwrap();
    m.set_settings(settings(files.path(), 6));
    let names: Vec<String> = (0..18).map(|i| format!("file-{i:02}.bin")).collect();
    for n in &names {
        m.add(NewDownload { urls: vec![srv.url()], file_name: Some(n.clone()), ..NewDownload::default() })
            .unwrap();
    }
    wait_for(&m, 60, "all eighteen to finish", |m| m.list().iter().all(|i| i.status == Status::Completed))
        .await;
    let want = content(53, size);
    for n in &names {
        assert!(std::fs::read(files.path().join(n)).unwrap() == want, "{n} is damaged");
    }
    let stray: Vec<_> = std::fs::read_dir(files.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".weft"))
        .collect();
    assert!(stray.is_empty(), "left behind: {stray:?}");
}

/// Pausing and resuming over and over, faster than a person could, loses nothing
/// and does not fetch the file several times over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_storm_keeps_the_file_exact() {
    let size = 12 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 59, rate_total: Some(4 * MIB), ..Default::default() }).await;
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let m = Manager::open(state.path(), files.path()).unwrap();
    m.set_settings(settings(files.path(), 2));
    let id = m
        .add(NewDownload {
            urls: vec![srv.url()],
            file_name: Some("storm.bin".into()),
            ..NewDownload::default()
        })
        .unwrap();
    for i in 0..14u64 {
        tokio::time::sleep(Duration::from_millis(90 + 37 * (i % 4))).await;
        m.pause(id);
        tokio::time::sleep(Duration::from_millis(20 + 45 * (i % 3))).await;
        m.resume(id);
    }
    wait_for(&m, 40, "the download to finish", |m| m.list().iter().all(|i| i.status == Status::Completed))
        .await;
    assert_same(&files.path().join("storm.bin"), 59, size);
    let sent = srv.stats().body_bytes.load(Ordering::Relaxed);
    assert!(sent < size * 2, "sent {sent} bytes for a {size} byte file");
}

/// Offsets past 4 GiB: nothing may be kept in 32 bits anywhere between the
/// request, the scheduler, the journal and the disk. Slow; run with `--ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "writes 4.2 GiB; run with --release -- --ignored"]
async fn file_larger_than_4_gib_is_exact() {
    let size = (4 << 30) + 187 * MIB + 12_345;
    let srv = Server::local(Behavior { size, seed: 61, ..Default::default() }).await;
    let dir = tempfile::tempdir_in(std::env::var("WEFT_TEST_DIR").unwrap_or_else(|_| ".".into())).unwrap();
    let (result, _) = run(spec(dir.path(), &srv.url()), Config::default(), vec![Route::system()]).await;
    let Ok(Outcome::Completed { path, size: got }) = result else { panic!("{result:?}") };
    assert_eq!(got, size);
    assert_same(&path, 61, size);
}
