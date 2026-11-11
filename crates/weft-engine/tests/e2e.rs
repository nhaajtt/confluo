//! End-to-end tests: the real engine against a server that misbehaves on purpose.
//! Every test that completes compares the file byte for byte.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::watch;
use url::Url;
use weft_engine::limit::Limiter;
use weft_engine::sched::SchedConfig;
use weft_engine::{Checksum, Command, Config, Error, Outcome, Route, Snapshot, Spec, Verdict, download};
use weft_testsrv::{Behavior, Server, content};

const MIB: u64 = 1 << 20;

fn cfg() -> Config {
    Config {
        stall_timeout: Duration::from_secs(1),
        sched: SchedConfig {
            min_piece: 64 * 1024,
            first_piece: 128 * 1024,
            min_steal: 64 * 1024,
            target_secs: 1.0,
            min_steal_eta_secs: 1.0,
            ..Default::default()
        },
        write_chunk: 64 * 1024,
        journal_interval: Duration::from_millis(100),
        tick: Duration::from_millis(50),
        give_up_after: Some(Duration::from_secs(20)),
        ..Default::default()
    }
}

fn spec(dir: &Path, urls: &[String]) -> Spec {
    Spec {
        urls: urls.iter().map(|u| Url::parse(u).unwrap()).collect(),
        dir: dir.to_path_buf(),
        file_name: None,
        headers: Vec::new(),
        checksum: None,
    }
}

fn limited(id: &str, rate: u64) -> Route {
    Route { id: id.into(), label: id.into(), bind: None, limiter: Arc::new(Limiter::new(rate)), uplink: None }
}

struct Run {
    result: Result<Outcome, Error>,
    snaps: Vec<Snapshot>,
}

async fn run_with(spec: Spec, cfg: Config, routes: Vec<Route>) -> Run {
    let (_rtx, rrx) = watch::channel(routes);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut snaps = Vec::new();
    let result = download(spec, cfg, rrx, crx, &mut |s| snaps.push(s)).await;
    Run { result, snaps }
}

async fn run(spec: Spec) -> Run {
    run_with(spec, cfg(), vec![Route::system()]).await
}

fn assert_file(run: &Run, seed: u64, size: u64) -> std::path::PathBuf {
    let Ok(Outcome::Completed { path, size: got }) = &run.result else {
        panic!("not completed: {:?}", run.result)
    };
    assert_eq!(*got, size);
    let data = std::fs::read(path).unwrap();
    assert!(data == content(seed, size), "file content differs");
    let dir = path.parent().unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".weft"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    path.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranged_download_is_exact() {
    let size = 8 * MIB + 12345;
    let srv =
        Server::local(Behavior { size, seed: 11, filename: Some("data.bin".into()), ..Default::default() })
            .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    let path = assert_file(&r, 11, size);
    assert_eq!(path.file_name().unwrap(), "data.bin");
    // The server counts what it pushed into the socket. The only bytes beyond the file are
    // the probe and what was still in flight when a connection handed over its tail.
    let sent = srv.stats().body_bytes.load(Ordering::Relaxed);
    assert!(sent <= size + size / 20, "sent {sent} for a {size} byte file");
    assert!(srv.stats().peak_active.load(Ordering::Relaxed) > 1, "expected parallel connections");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_download_of_same_name_does_not_overwrite() {
    let srv = Server::local(Behavior { size: 100_000, seed: 3, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let a = run(spec(dir.path(), &[srv.url()])).await;
    let b = run(spec(dir.path(), &[srv.url()])).await;
    let (Ok(Outcome::Completed { path: pa, .. }), Ok(Outcome::Completed { path: pb, .. })) =
        (&a.result, &b.result)
    else {
        panic!()
    };
    assert_ne!(pa, pb);
    assert_eq!(pb.file_name().unwrap(), "file (1)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_without_ranges_uses_one_stream() {
    let size = 3 * MIB + 7;
    let srv = Server::local(Behavior { size, seed: 5, ranges: false, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 5, size);
    assert!(r.snaps.iter().all(|s| s.connections <= 1));
    assert!(r.snaps.iter().any(|s| s.verdict == Verdict::ServerSingleStream && !s.resumable));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_without_ranges_restarts_after_a_drop() {
    let size = 2 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 6,
        ranges: false,
        drop_after: Some(700_000),
        drop_count: 2,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 6, size);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_length_stream() {
    let size = MIB + 99;
    let srv =
        Server::local(Behavior { size, seed: 8, ranges: false, content_length: false, ..Default::default() })
            .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 8, size);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_file() {
    let srv = Server::local(Behavior { size: 0, seed: 1, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 1, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_dropped_and_stalled_connections() {
    let size = 6 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 21,
        drop_after: Some(90_000),
        drop_count: 12,
        stall_after: Some(50_000),
        stall_count: 4,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 21, size);
    // Bytes received before a drop are kept, so the overhead stays small.
    let sent = srv.stats().body_bytes.load(Ordering::Relaxed);
    assert!(sent < size + size / 4, "sent {sent} for {size}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_server_is_respected() {
    let size = 4 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 31,
        max_conns: Some(2),
        rate_per_conn: Some(2 * MIB),
        fail_status: Some(503),
        fail_count: 0,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 31, size);
    assert!(r.snaps.iter().any(|s| s.verdict == Verdict::ServerRefusing), "refusal was not diagnosed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_connection_cap_is_beaten_with_more_connections() {
    let size = 12 * MIB;
    // One connection would need 60 s.
    let srv =
        Server::local(Behavior { size, seed: 41, rate_per_conn: Some(200 * 1024), ..Default::default() })
            .await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert_file(&r, 41, size);
    let secs = t.elapsed().as_secs_f64();
    assert!(secs < 12.0, "took {secs:.1}s");
    assert!(r.snaps.iter().any(|s| s.verdict == Verdict::ServerPerConnection), "cap was not diagnosed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume_fetches_only_the_rest() {
    let size = 6 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 51, rate_total: Some(2 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (ctx, crx) = watch::channel(Command::Run);
    let mut seen = 0u64;
    let first = download(spec(dir.path(), &[srv.url()]), cfg(), rrx.clone(), crx, &mut |s| {
        seen = s.done;
        if s.done > 2 * MIB {
            let _ = ctx.send(Command::Pause);
        }
    })
    .await;
    assert_eq!(first.unwrap(), Outcome::Paused);
    assert!(seen > 2 * MIB && seen < size);
    let before = srv.stats().body_bytes.load(Ordering::Relaxed);
    srv.behavior(|b| b.rate_total = None);
    let (_c2, crx2) = watch::channel(Command::Run);
    let mut first_done = None;
    let second = download(spec(dir.path(), &[srv.url()]), cfg(), rrx, crx2, &mut |s| {
        first_done.get_or_insert(s.done);
    })
    .await;
    let r = Run { result: second, snaps: Vec::new() };
    assert_file(&r, 51, size);
    let again = srv.stats().body_bytes.load(Ordering::Relaxed) - before;
    assert!(again <= size - seen + 256 * 1024, "resume fetched {again}, only {} were missing", size - seen);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_mid_download_resumes_from_the_journal() {
    let size = 6 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 61, rate_total: Some(3 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let s = spec(dir.path(), &[srv.url()]);
    // Dropping the future mid-flight is the in-process equivalent of a crash:
    // no pause, no final flush, no final journal.
    let killed = tokio::time::timeout(Duration::from_millis(1200), run(s.clone())).await;
    assert!(killed.is_err(), "download finished before it could be killed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let journal = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".weft-journal"));
    assert!(journal, "no journal survived the kill");
    let before = srv.stats().body_bytes.load(Ordering::Relaxed);
    srv.behavior(|b| b.rate_total = None);
    let r = run(s).await;
    assert_file(&r, 61, size);
    let again = srv.stats().body_bytes.load(Ordering::Relaxed) - before;
    assert!(again < size, "nothing was recovered: fetched {again} of {size} again");
    // The journal is flushed several times a second, so about a second of work
    // (3 MiB here) is the most a crash can cost.
    assert!(before + again < size + 3 * MIB, "lost too much: {before} + {again} for {size}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_changing_on_the_server_is_detected() {
    let size = 6 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 71, rate_total: Some(2 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut changed = false;
    let result = download(spec(dir.path(), &[srv.url()]), cfg(), rrx, crx, &mut |s| {
        if s.done > MIB && !changed {
            changed = true;
            srv.behavior(|b| {
                b.seed = 72;
                b.etag = Some("\"v2\"".into());
                b.rate_total = None;
            });
        }
    })
    .await;
    assert!(matches!(result, Err(Error::FileChanged)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_signed_link_is_refreshed() {
    let size = 6 * MIB;
    let srv = Server::local(Behavior {
        size,
        seed: 81,
        token: Some("first".into()),
        rate_total: Some(2 * MIB),
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut expired = false;
    let result = download(spec(dir.path(), &[srv.link_url()]), cfg(), rrx, crx, &mut |s| {
        if s.done > MIB && !expired {
            expired = true;
            srv.behavior(|b| {
                b.token = Some("second".into());
                b.rate_total = None;
            });
        }
    })
    .await;
    assert!(expired);
    assert_file(&Run { result, snaps: Vec::new() }, 81, size);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checksum_catches_a_corrupted_byte() {
    let size = 2 * MIB;
    let hex: String = Sha256::digest(content(91, size)).iter().map(|b| format!("{b:02x}")).collect();
    let srv = Server::local(Behavior {
        size,
        seed: 91,
        corrupt_at: Some(size / 2),
        corrupt_count: 1,
        ..Default::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = spec(dir.path(), &[srv.url()]);
    s.checksum = Checksum::parse(&hex);
    let bad = run(s.clone()).await;
    assert!(matches!(bad.result, Err(Error::Integrity { .. })), "{:?}", bad.result);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "a corrupt file was left behind");
    let good = run(s).await;
    assert_file(&good, 91, size);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_advertised_checksum_is_verified() {
    let size = MIB;
    let srv =
        Server::local(Behavior { size, seed: 92, sha256_hex: Some("00".repeat(32)), ..Default::default() })
            .await;
    let dir = tempfile::tempdir().unwrap();
    let r = run(spec(dir.path(), &[srv.url()])).await;
    assert!(matches!(r.result, Err(Error::Integrity { .. })), "{:?}", r.result);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mirrors_share_the_work_and_wrong_mirrors_are_skipped() {
    let size = 8 * MIB;
    let cap = Some(MIB);
    let a = Server::local(Behavior { size, seed: 101, rate_total: cap, ..Default::default() }).await;
    let b = Server::local(Behavior {
        size,
        seed: 101,
        rate_total: cap,
        etag: Some("\"other\"".into()),
        ..Default::default()
    })
    .await;
    let wrong = Server::local(Behavior { size, seed: 999, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let r = run(spec(dir.path(), &[a.url(), b.url(), wrong.url()])).await;
    assert_file(&r, 101, size);
    let (sa, sb, sw) = (
        a.stats().body_bytes.load(Ordering::Relaxed),
        b.stats().body_bytes.load(Ordering::Relaxed),
        wrong.stats().body_bytes.load(Ordering::Relaxed),
    );
    assert!(sa > size / 4 && sb > size / 4, "work was not shared: {sa} / {sb}");
    assert!(sw < 64 * 1024, "the wrong mirror was used: {sw}");
    assert!(r.snaps.iter().any(|s| s.sources == 2));
    assert!(r.snaps.last().unwrap().notes.iter().any(|n| n.contains("different file")));
    // Two 1 MiB/s servers: clearly faster than the 8 s one of them would need.
    assert!(t.elapsed().as_secs_f64() < 6.5, "{:?}", t.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn faster_route_carries_more_and_slow_route_does_not_hold_the_tail() {
    let size = 9 * MIB;
    let srv = Server::local(Behavior { size, seed: 111, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let r = run_with(
        spec(dir.path(), &[srv.url()]),
        cfg(),
        vec![limited("fast", 4 * MIB), limited("slow", MIB / 2)],
    )
    .await;
    assert_file(&r, 111, size);
    let last = r.snaps.last().unwrap();
    let fast = last.routes.iter().find(|r| r.id == "fast").unwrap().bytes;
    let slow = last.routes.iter().find(|r| r.id == "slow").unwrap().bytes;
    assert!(fast > slow * 4, "fast {fast}, slow {slow}");
    assert!(slow > 0, "the slow route was never used");
    // 4.5 MiB/s combined gives 2 s; a naive even split would wait 9 s for the slow half.
    assert!(t.elapsed().as_secs_f64() < 4.0, "{:?}", t.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_route_does_not_stop_the_download() {
    let size = 4 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 121, rate_total: Some(4 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let dead = Route {
        id: "dead".into(),
        label: "dead".into(),
        bind: Some(weft_net::BindSpec {
            index: 0x00FF_FFF0,
            name: "nope0".into(),
            v4: Some([203, 0, 113, 9].into()),
            v6: None,
        }),
        limiter: Arc::default(),
        uplink: None,
    };
    let r = run_with(spec(dir.path(), &[srv.url()]), cfg(), vec![dead, Route::system()]).await;
    assert_file(&r, 121, size);
    let seen_retrying = r.snaps.iter().any(|s| {
        s.routes.iter().any(|r| r.id == "dead" && r.health == weft_engine::download::RouteHealth::Retrying)
    });
    assert!(seen_retrying, "the dead route was never reported as failing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_removed_mid_download_fails_over() {
    let size = 6 * MIB;
    let srv = Server::local(Behavior { size, seed: 131, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let (rtx, rrx) = watch::channel(vec![limited("a", 2 * MIB), limited("b", 2 * MIB)]);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut removed = false;
    let mut after: Vec<usize> = Vec::new();
    let result = download(spec(dir.path(), &[srv.url()]), cfg(), rrx, crx, &mut |s| {
        if s.done > MIB && !removed {
            removed = true;
            let _ = rtx.send(vec![limited("b", 2 * MIB)]);
        } else if removed {
            after.push(s.routes.len());
        }
    })
    .await;
    assert_file(&Run { result, snaps: Vec::new() }, 131, size);
    assert!(removed && after.iter().skip(2).all(|n| *n == 1), "{after:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_file_name_stays_in_the_folder() {
    let srv = Server::local(Behavior {
        size: 1000,
        seed: 141,
        filename: Some("..\\\\..\\\\evil.txt".into()),
        ..Default::default()
    })
    .await;
    let outer = tempfile::tempdir().unwrap();
    let dir = outer.path().join("a").join("b");
    let r = run(spec(&dir, &[srv.url()])).await;
    let path = assert_file(&r, 141, 1000);
    assert_eq!(path.parent().unwrap(), dir);
    assert!(!outer.path().join("evil.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_file_fails_fast_and_cancel_cleans_up() {
    let srv =
        Server::local(Behavior { size: 4 * MIB, seed: 151, rate_total: Some(MIB), ..Default::default() })
            .await;
    let dir = tempfile::tempdir().unwrap();
    let missing = format!("http://{}/nothing-here", srv.addr);
    let r = run(spec(dir.path(), &[missing])).await;
    assert!(matches!(r.result, Err(Error::Status { code: 404, .. })), "{:?}", r.result);

    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (ctx, crx) = watch::channel(Command::Run);
    let result = download(spec(dir.path(), &[srv.url()]), cfg(), rrx, crx, &mut |s| {
        if s.done > 300_000 {
            let _ = ctx.send(Command::Cancel);
        }
    })
    .await;
    assert_eq!(result.unwrap(), Outcome::Cancelled);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirects_are_followed_and_user_limit_is_obeyed() {
    let size = 3 * MIB;
    let srv = Server::local(Behavior { size, seed: 161, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg();
    c.limiter = Arc::new(Limiter::new(MIB));
    let t = Instant::now();
    let r =
        run_with(spec(dir.path(), &[format!("http://{}/redirect/3", srv.addr)]), c, vec![Route::system()])
            .await;
    assert_file(&r, 161, size);
    let secs = t.elapsed().as_secs_f64();
    assert!((2.3..5.0).contains(&secs), "3 MiB at 1 MiB/s took {secs:.2}s");
    assert!(r.snaps.iter().any(|s| s.verdict == Verdict::UserLimit));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_settings_use_few_requests() {
    let size = 64 * MIB;
    let srv = Server::local(Behavior { size, seed: 171, ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let c = Config { give_up_after: Some(Duration::from_secs(20)), ..Default::default() };
    let r = run_with(spec(dir.path(), &[srv.url()]), c, vec![Route::system()]).await;
    assert_file(&r, 171, size);
    let requests = srv.stats().requests.load(Ordering::Relaxed);
    assert!(requests <= 40, "{requests} requests for a 64 MiB file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn too_many_requests_switches_to_long_requests() {
    let size = 24 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 181, rate_total: Some(6 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (_ctx, crx) = watch::channel(Command::Run);
    let mut tripped = false;
    let mut noted = false;
    let mut requests_at_trip = 0;
    let result = download(spec(dir.path(), &[srv.url()]), cfg(), rrx, crx, &mut |s| {
        if s.done > 2 * MIB && !tripped {
            tripped = true;
            requests_at_trip = srv.stats().requests.load(Ordering::Relaxed);
            srv.behavior(|b| {
                b.fail_status = Some(429);
                b.fail_count = 3;
            });
        }
        noted |= s.notes.iter().any(|n| n.contains("limits requests"));
    })
    .await;
    assert_file(&Run { result, snaps: Vec::new() }, 181, size);
    assert!(noted, "the request limit was not reported");
    // With 64 KiB pieces the remaining 22 MiB would take hundreds of requests.
    let after = srv.stats().requests.load(Ordering::Relaxed) - requests_at_trip;
    assert!(after < 60, "{after} requests after the server asked for fewer");
}
