//! The engine against servers that lie or never stop talking. The test server is
//! too well-behaved to play these parts, so each test scripts its own replies.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use confluo_engine::http::RequestOptions;
use confluo_engine::sched::SchedConfig;
use confluo_engine::{Command, Config, Error, Outcome, Route, Spec, download, probe};
use confluo_testsrv::{Behavior, Server, content};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use url::Url;

const MIB: u64 = 1 << 20;

/// What to send after the response head.
enum Body {
    /// These bytes, then close the connection.
    Bytes(Vec<u8>),
    /// The same byte for ever, at about this many bytes per second.
    Endless(u64),
}

/// The byte range a request asks for, as written: `bytes=a-` or `bytes=a-b`.
fn asked_range(head: &str) -> Option<(u64, Option<u64>)> {
    let line = head.lines().find(|l| l.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split_once("bytes=")?.1.trim();
    let (a, b) = spec.split_once('-')?;
    Some((a.parse().ok()?, b.trim().parse().ok()))
}

struct Raw {
    addr: SocketAddr,
    /// Body bytes written so far, over every connection.
    sent: Arc<AtomicU64>,
}

impl Raw {
    fn url(&self) -> String {
        format!("http://{}/file.bin", self.addr)
    }
}

/// Serves every request with `respond(request_head, request_number)`.
async fn raw<F>(respond: F) -> Raw
where
    F: Fn(&str, u64) -> (String, Body) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sent = Arc::new(AtomicU64::new(0));
    let respond = Arc::new(respond);
    let counter = Arc::new(AtomicU64::new(0));
    let total = sent.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (respond, counter, total) = (respond.clone(), counter.clone(), total.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let n = counter.fetch_add(1, Ordering::Relaxed);
                let (reply, body) = respond(&String::from_utf8_lossy(&head), n);
                if sock.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                match body {
                    Body::Bytes(b) => {
                        if sock.write_all(&b).await.is_ok() {
                            total.fetch_add(b.len() as u64, Ordering::Relaxed);
                        }
                    }
                    Body::Endless(rate) => {
                        let block = vec![b'x'; 64 * 1024];
                        let pause = Duration::from_secs_f64(block.len() as f64 / rate as f64);
                        while sock.write_all(&block).await.is_ok() {
                            total.fetch_add(block.len() as u64, Ordering::Relaxed);
                            tokio::time::sleep(pause).await;
                        }
                    }
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    Raw { addr, sent }
}

fn cfg() -> Config {
    Config {
        stall_timeout: Duration::from_secs(1),
        sched: SchedConfig {
            min_piece: 64 * 1024,
            first_piece: 128 * 1024,
            equal_split: false,
            ..Default::default()
        },
        write_chunk: 64 * 1024,
        journal_interval: Duration::from_millis(100),
        tick: Duration::from_millis(50),
        give_up_after: Some(Duration::from_secs(10)),
        ..Default::default()
    }
}

fn spec(dir: &std::path::Path, urls: &[String]) -> Spec {
    Spec {
        urls: urls.iter().map(|u| Url::parse(u).unwrap()).collect(),
        dir: dir.to_path_buf(),
        file_name: None,
        headers: Vec::new(),
        checksum: None,
    }
}

async fn run(spec: Spec) -> Result<Outcome, Error> {
    let (_rtx, rrx) = watch::channel(vec![Route::system()]);
    let (_ctx, crx) = watch::channel(Command::Run);
    download(spec, cfg(), rrx, crx, &mut |_| {}).await
}

/// A 206 whose range ends at the largest possible byte must not make the
/// download ask for the same bytes for ever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn range_ending_at_the_last_possible_byte_does_not_spin() {
    let size = 2 * MIB;
    let data = Arc::new(content(31, size));
    let served = data.clone();
    let srv = raw(move |head, _| {
        let (start, end) = asked_range(head).unwrap_or((0, None));
        match end {
            // The opening request gets an honest answer that breaks off early, so
            // the rest has to be asked for in ranges.
            None => (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{size}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    size - 1,
                    size - start
                ),
                Body::Bytes(served[start as usize..(start + 64 * 1024) as usize].to_vec()),
            ),
            Some(end) => (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{size}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    u64::MAX,
                    end + 1 - start
                ),
                Body::Bytes(served[start as usize..=end as usize].to_vec()),
            ),
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), run(spec(dir.path(), &[srv.url()])))
        .await
        .expect("the download kept asking for the same bytes");
    match result {
        Err(Error::Protocol(_)) => {}
        Ok(Outcome::Completed { path, .. }) => assert_eq!(std::fs::read(path).unwrap(), *data),
        other => panic!("unexpected: {other:?}"),
    }
}

/// The one-byte probe answer is read to its end only if it really is tiny.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endless_probe_answer_is_not_read_into_memory() {
    let srv = raw(|_, _| {
        (
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000000\r\n\r\n".into(),
            Body::Endless(64 * MIB),
        )
    })
    .await;
    let url = Url::parse(&srv.url()).unwrap();
    let opts = RequestOptions {
        user_agent: "test".into(),
        headers: Vec::new(),
        secrets_for: None,
        connect_timeout: Duration::from_secs(5),
        header_timeout: Duration::from_secs(5),
    };
    let p = probe::probe(None, &url, &opts).await.unwrap();
    assert_eq!(p.size, Some(1_000_000));
    let sent = srv.sent.load(Ordering::Relaxed);
    assert!(sent < 8 * MIB, "read {} MiB of a one-byte answer", sent / MIB);
}

/// A mirror whose sample answer never ends is skipped without being read into memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endless_mirror_sample_is_not_read_into_memory() {
    let size = 4 * MIB;
    let good = Server::local(Behavior { size, seed: 32, ..Default::default() }).await;
    let bad = raw(move |head, _| {
        let (start, end) = asked_range(head).unwrap_or((0, None));
        let end = end.unwrap_or(size - 1);
        (
            format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{size}\r\n\r\n"),
            Body::Endless(64 * MIB),
        )
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let result = run(spec(dir.path(), &[good.url(), bad.url()])).await;
    let Ok(Outcome::Completed { path, .. }) = result else { panic!("{result:?}") };
    assert_eq!(std::fs::read(path).unwrap(), content(32, size));
    let sent = bad.sent.load(Ordering::Relaxed);
    assert!(sent < 16 * MIB, "read {} MiB from a mirror sample", sent / MIB);
}

/// Two downloads of the same link into the same folder at the same time each get
/// their own file, both exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_link_twice_at_once_gives_two_exact_files() {
    let size = 6 * MIB;
    let srv =
        Server::local(Behavior { size, seed: 33, rate_total: Some(8 * MIB), ..Default::default() }).await;
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = tokio::join!(run(spec(dir.path(), &[srv.url()])), run(spec(dir.path(), &[srv.url()])));
    let want = content(33, size);
    let (Ok(Outcome::Completed { path: pa, .. }), Ok(Outcome::Completed { path: pb, .. })) = (&a, &b) else {
        panic!("{a:?} / {b:?}")
    };
    assert_ne!(pa, pb);
    assert_eq!(std::fs::read(pa).unwrap(), want);
    assert_eq!(std::fs::read(pb).unwrap(), want);
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".confluo"))
        .count();
    assert_eq!(leftovers, 0);
}
