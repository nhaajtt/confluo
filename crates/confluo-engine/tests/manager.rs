//! The download list: queueing, limits on how many run at once, restart behaviour.

use std::time::Duration;

use confluo_engine::manager::{Event, Manager, Networks, NewDownload, Settings, Status};
use confluo_testsrv::{Behavior, Server, content};

const MIB: u64 = 1 << 20;

fn settings(dir: &std::path::Path, max: usize) -> Settings {
    Settings {
        download_dir: dir.into(),
        max_concurrent: max,
        networks: Networks::System,
        ..Settings::default()
    }
}

async fn wait_for(m: &Manager, what: &str, pred: impl Fn(&Manager) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !pred(m) {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}: {:#?}", m.list());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn new(url: String, name: &str) -> NewDownload {
    NewDownload { urls: vec![url], file_name: Some(name.into()), ..NewDownload::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_in_order_and_never_more_than_allowed() {
    let srv =
        Server::local(Behavior { size: 2 * MIB, seed: 7, rate_total: Some(4 * MIB), ..Default::default() })
            .await;
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let m = Manager::open(state.path(), files.path()).unwrap();
    m.set_settings(settings(files.path(), 1));
    let mut events = m.subscribe();
    let ids: Vec<u64> =
        ["a.bin", "b.bin", "c.bin"].iter().map(|n| m.add(new(srv.url(), n)).unwrap()).collect();

    // Move the last one to the front before it starts: it must run second
    // (the first is already running) and the middle one last.
    m.move_to(ids[2], 0);
    let watcher = tokio::spawn(async move {
        let mut most = 0usize;
        let mut running = std::collections::HashSet::new();
        while let Ok(ev) = events.recv().await {
            if let Event::Item(item) = ev {
                if item.status == Status::Running {
                    running.insert(item.id);
                } else {
                    running.remove(&item.id);
                }
                most = most.max(running.len());
                if item.status == Status::Completed && item.file_name == "b.bin" {
                    break;
                }
            }
        }
        most
    });
    wait_for(&m, "all three to finish", |m| m.list().iter().all(|i| i.status == Status::Completed)).await;
    assert_eq!(watcher.await.unwrap(), 1, "more than one download ran at once");
    let list = m.list();
    let finished = |id: u64| list.iter().find(|i| i.id == id).unwrap().finished_at.unwrap();
    assert!(finished(ids[0]) <= finished(ids[2]) && finished(ids[2]) <= finished(ids[1]));
    for name in ["a.bin", "b.bin", "c.bin"] {
        assert!(std::fs::read(files.path().join(name)).unwrap() == content(7, 2 * MIB));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_remove_and_restart() {
    let srv =
        Server::local(Behavior { size: 8 * MIB, seed: 9, rate_total: Some(2 * MIB), ..Default::default() })
            .await;
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let cookie = ("Cookie".to_string(), "session=top-secret-value".to_string());
    let (keep, drop_me);
    {
        let m = Manager::open(state.path(), files.path()).unwrap();
        m.set_settings(settings(files.path(), 2));
        keep = m
            .add(NewDownload {
                headers: vec![cookie.clone(), ("Referer".into(), "http://x/".into())],
                ..new(srv.url(), "keep.bin")
            })
            .unwrap();
        drop_me = m.add(new(srv.url(), "drop.bin")).unwrap();
        // The kept download gets half the file first, so that "did not start over" can be
        // told apart from the bytes a server pushes into sockets that are then closed.
        wait_for(&m, "both to receive data", |m| {
            m.list().iter().all(|i| i.done > 256 * 1024)
                && m.list().iter().any(|i| i.id == keep && i.done > 4 * MIB)
        })
        .await;

        m.pause(keep);
        wait_for(&m, "pause", |m| m.list().iter().any(|i| i.id == keep && i.status == Status::Paused)).await;
        m.remove(drop_me, true);
        wait_for(&m, "removal", |m| m.list().len() == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!files.path().join("drop.bin.confluo").exists(), "a removed download left its partial file");
        assert!(files.path().join("keep.bin.confluo").exists());
    }
    // The saved list never holds the cookie in the clear.
    let saved = std::fs::read_to_string(state.path().join("downloads.json")).unwrap();
    assert!(!saved.contains("top-secret-value"), "cookie written in plain text");
    assert!(saved.contains("Referer"));

    // A new process: the paused download is still there, with its progress.
    srv.behavior(|b| b.rate_total = None);
    let m = Manager::open(state.path(), files.path()).unwrap();
    let list = m.list();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id, list[0].status), (keep, Status::Paused));
    let before = list[0].done;
    assert!(before > 0);
    let sent_before = srv.stats().body_bytes.load(std::sync::atomic::Ordering::Relaxed);
    m.resume(keep);
    wait_for(&m, "completion after restart", |m| m.list()[0].status == Status::Completed).await;
    assert!(std::fs::read(files.path().join("keep.bin")).unwrap() == content(9, 8 * MIB));
    let again = srv.stats().body_bytes.load(std::sync::atomic::Ordering::Relaxed) - sent_before;
    // What the server counts as sent includes data left in the buffers of connections that
    // were handed over mid-response; on a fast loopback that is up to a few megabytes.
    let allowed = 8 * MIB - before + 3 * MIB;
    assert!(again < allowed, "the restart fetched {again} bytes with {before} already on disk");

    assert!(m.add(new("ftp://example.com/x".into(), "x")).is_err());
    assert!(m.add(new("file:///C:/Windows/win.ini".into(), "x")).is_err());
    assert!(m.add(NewDownload { checksum: Some("nope".into()), ..new(srv.url(), "y") }).is_err());
    m.clear_completed();
    assert!(m.list().is_empty());
}

/// A list file that cannot be read is kept aside instead of being silently replaced,
/// and a tampered connection count cannot make the engine open thousands of sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn damaged_or_hostile_saved_state_is_contained() {
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("downloads.json"), b"{ this is not json").unwrap();
    let m = Manager::open(state.path(), files.path()).unwrap();
    assert!(m.list().is_empty());
    assert!(state.path().join("downloads.json.unreadable").exists(), "the damaged list was thrown away");
    drop(m);

    let hostile = serde_json::json!({
        "settings": { "download_dir": files.path(), "max_concurrent": 9000, "connections_per_network": 4_000_000_000u64,
                      "speed_limit": 0, "network_limits": {}, "networks": { "mode": "system" } },
        "next_id": 1,
        "items": []
    });
    std::fs::write(state.path().join("downloads.json"), serde_json::to_vec(&hostile).unwrap()).unwrap();
    let m = Manager::open(state.path(), files.path()).unwrap();
    assert!(m.settings().max_concurrent <= 16);
    assert!(m.settings().connections_per_network <= 64);
}

/// Many changes at once from many threads: the list on disk must always be whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_saves_never_leave_a_broken_list() {
    let state = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let m = std::sync::Arc::new(Manager::open(state.path(), files.path()).unwrap());
    m.set_settings(settings(files.path(), 1));
    let mut tasks = Vec::new();
    for t in 0..8 {
        let m = m.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            for i in 0..25 {
                let id = m
                    .add(NewDownload {
                        urls: vec![format!("http://127.0.0.1:9/never-{t}-{i}")],
                        file_name: Some(format!("n-{t}-{i}.bin")),
                        paused: true,
                        ..NewDownload::default()
                    })
                    .unwrap();
                if i % 3 == 0 {
                    m.remove(id, false);
                }
            }
        }));
    }
    let file = state.path().join("downloads.json");
    let reader = tokio::task::spawn_blocking({
        let file = file.clone();
        move || {
            let mut read = 0;
            for _ in 0..400 {
                if let Ok(bytes) = std::fs::read(&file) {
                    assert!(
                        serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
                        "a half-written list was visible"
                    );
                    read += 1;
                }
            }
            read
        }
    });
    for t in tasks {
        t.await.unwrap();
    }
    assert!(reader.await.unwrap() > 0);
    let kept = m.list().len();
    assert_eq!(kept, 8 * 25 - 8 * 9);
    let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(on_disk["items"].as_array().unwrap().len(), kept, "the last save lost changes");
}
