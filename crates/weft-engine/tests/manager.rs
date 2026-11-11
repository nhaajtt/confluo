//! The download list: queueing, limits on how many run at once, restart behaviour.

use std::time::Duration;

use weft_engine::manager::{Event, Manager, Networks, NewDownload, Settings, Status};
use weft_testsrv::{Behavior, Server, content};

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
        wait_for(&m, "both to receive data", |m| m.list().iter().all(|i| i.done > 256 * 1024)).await;

        m.pause(keep);
        wait_for(&m, "pause", |m| m.list().iter().any(|i| i.id == keep && i.status == Status::Paused)).await;
        m.remove(drop_me, true);
        wait_for(&m, "removal", |m| m.list().len() == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!files.path().join("drop.bin.weft").exists(), "a removed download left its partial file");
        assert!(files.path().join("keep.bin.weft").exists());
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
    assert!(again < 8 * MIB, "the restart downloaded everything again ({again} bytes)");

    assert!(m.add(new("ftp://example.com/x".into(), "x")).is_err());
    assert!(m.add(new("file:///C:/Windows/win.ini".into(), "x")).is_err());
    assert!(m.add(NewDownload { checksum: Some("nope".into()), ..new(srv.url(), "y") }).is_err());
    m.clear_completed();
    assert!(m.list().is_empty());
}
