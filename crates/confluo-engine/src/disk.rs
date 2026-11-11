//! The partial file and its single writer thread.
//!
//! All connections hand their data to one thread that owns one file handle and
//! writes at explicit offsets. The queue between them is bounded, so a slow disk
//! slows the sockets down instead of filling memory.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::ranges::RangeSet;

/// Messages queued for the writer; each holds at most one coalesced buffer.
const QUEUE_SLOTS: usize = 64;

enum Cmd {
    Write {
        offset: u64,
        data: Bytes,
    },
    /// Flush to stable storage and report which bytes are safely on disk.
    Sync(oneshot::Sender<io::Result<RangeSet>>),
    Truncate(oneshot::Sender<io::Result<()>>),
    /// Close the file and stop the thread.
    Close(oneshot::Sender<()>),
}

#[derive(Debug, Default)]
pub struct DiskStats {
    /// Bytes waiting in the queue.
    pub queued: AtomicU64,
    pub written: AtomicU64,
    /// Nanoseconds spent inside write calls.
    pub write_nanos: AtomicU64,
}

#[derive(Clone)]
pub struct DiskWriter {
    tx: mpsc::Sender<Cmd>,
    pub stats: Arc<DiskStats>,
    done: Arc<Mutex<RangeSet>>,
    error: Arc<Mutex<Option<io::Error>>>,
}

fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_write(file, buf, offset)?;
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

impl DiskWriter {
    /// Starts the writer thread. `done` is the set of bytes already on disk (from a journal).
    pub fn start(file: File, done: RangeSet) -> Self {
        let (tx, mut rx) = mpsc::channel::<Cmd>(QUEUE_SLOTS);
        let stats = Arc::new(DiskStats::default());
        let done = Arc::new(Mutex::new(done));
        let error = Arc::new(Mutex::new(None));
        let (t_stats, t_done, t_error) = (stats.clone(), done.clone(), error.clone());
        std::thread::Builder::new()
            .name("confluo-disk".into())
            .spawn(move || {
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        Cmd::Write { offset, data } => {
                            let len = data.len() as u64;
                            let t = std::time::Instant::now();
                            let res = write_at(&file, &data, offset);
                            t_stats.write_nanos.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                            t_stats.queued.fetch_sub(len, Ordering::Relaxed);
                            match res {
                                Ok(()) => {
                                    t_done.lock().unwrap().insert(offset, offset + len);
                                    t_stats.written.fetch_add(len, Ordering::Relaxed);
                                }
                                Err(e) => {
                                    *t_error.lock().unwrap() = Some(e);
                                    // Closing the queue makes every sender fail fast.
                                    return;
                                }
                            }
                        }
                        Cmd::Sync(reply) => {
                            // Everything queued before this command is written (single thread),
                            // so the snapshot taken here is exactly what the sync makes durable.
                            let snapshot = t_done.lock().unwrap().clone();
                            let _ = reply.send(file.sync_data().map(|()| snapshot));
                        }
                        Cmd::Truncate(reply) => {
                            t_done.lock().unwrap().remove(0, u64::MAX);
                            let _ = reply.send(file.set_len(0));
                        }
                        Cmd::Close(reply) => {
                            drop(file);
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
            })
            .expect("spawn disk thread");
        Self { tx, stats, done, error }
    }

    fn failure(&self) -> io::Error {
        self.error
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "disk writer stopped"))
    }

    /// Queues a write. Waits when the disk is behind.
    pub async fn write(&self, offset: u64, data: Bytes) -> io::Result<()> {
        self.stats.queued.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.tx.send(Cmd::Write { offset, data }).await.map_err(|_| self.failure())
    }

    /// Flushes to stable storage. Returns the bytes that are now durable.
    pub async fn sync(&self) -> io::Result<RangeSet> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::Sync(tx)).await.map_err(|_| self.failure())?;
        rx.await.map_err(|_| self.failure())?
    }

    /// Empties the file and forgets all progress (used when a server cannot resume).
    pub async fn truncate(&self) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::Truncate(tx)).await.map_err(|_| self.failure())?;
        rx.await.map_err(|_| self.failure())?
    }

    /// Closes the file. Later writes fail. Returns once the handle is released.
    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Close(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Bytes written so far (not necessarily flushed).
    pub fn done(&self) -> RangeSet {
        self.done.lock().unwrap().clone()
    }

    pub fn done_bytes(&self) -> u64 {
        self.done.lock().unwrap().total()
    }

    /// Marks bytes as not present any more (they failed verification).
    pub fn forget(&self, start: u64, end: u64) {
        self.done.lock().unwrap().remove(start, end);
    }

    /// How full the queue is, 0.0..=1.0.
    pub fn pressure(&self) -> f64 {
        1.0 - self.tx.capacity() as f64 / QUEUE_SLOTS as f64
    }
}

/// Moves `from` over `to`, trying again for a moment when the move is refused.
///
/// On Windows a file cannot be replaced or renamed while another program holds it
/// open without allowing that: a virus scanner looking at a file that was just
/// written, an indexer, a backup tool, a script reading the list. Such holds last
/// milliseconds, so the move is repeated for up to about two seconds before the
/// error is passed on.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    let mut wait = std::time::Duration::from_millis(2);
    let mut waited = std::time::Duration::ZERO;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    && waited < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(wait);
                waited += wait;
                wait = (wait * 2).min(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Opens (or creates) the partial file and reserves space for it without zero-filling.
pub fn open_part(path: &Path, size: Option<u64>, fresh: bool) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(fresh).open(path)?;
    if let Some(size) = size
        && file.metadata()?.len() != size
    {
        reserve(&file, size)?;
    }
    Ok(file)
}

#[cfg(windows)]
fn reserve(file: &File, size: u64) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    // Without the sparse flag NTFS zero-fills everything below an out-of-order
    // write, which stalls the download for seconds on large files. File systems
    // that cannot do sparse files (exFAT, FAT32) refuse the call; that is fine.
    let mut returned = 0u32;
    // SAFETY: valid handle, no input or output buffers.
    unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        );
    }
    file.set_len(size)
}

#[cfg(unix)]
fn reserve(file: &File, size: u64) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd;
        // Real block allocation: no fragmentation and no late "disk full".
        // SAFETY: valid file descriptor.
        let rc = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, size as libc::off_t) };
        if rc == 0 {
            return Ok(());
        }
    }
    file.set_len(size)
}

/// Free bytes on the volume holding `path`, when the OS can tell.
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated and `free` is a valid out pointer.
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &raw mut free, std::ptr::null_mut(), std::ptr::null_mut())
        };
        (ok != 0).then_some(free)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is NUL-terminated and `s` is a valid out pointer.
        unsafe {
            let mut s: libc::statvfs = std::mem::zeroed();
            (libc::statvfs(c.as_ptr(), &raw mut s) == 0).then(|| s.f_bavail as u64 * s.f_frsize as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn out_of_order_writes_land_and_sync_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let file = open_part(&path, Some(1 << 20), true).unwrap();
        let w = DiskWriter::start(file, RangeSet::new());
        w.write(1000, Bytes::from_static(b"world")).await.unwrap();
        w.write(0, Bytes::from_static(b"hello")).await.unwrap();
        let durable = w.sync().await.unwrap();
        assert_eq!(durable.spans(), &[(0, 5), (1000, 1005)]);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), 1 << 20);
        assert_eq!(&data[0..5], b"hello");
        assert_eq!(&data[1000..1005], b"world");
        assert!(free_space(dir.path()).unwrap() > 0);
    }
}

#[cfg(all(test, windows))]
mod replace_tests {
    use super::replace_file;
    use std::os::windows::fs::OpenOptionsExt;

    /// Another program reads the destination without allowing it to be replaced
    /// (the way most programs open files). The move has to wait for it, not give up.
    #[test]
    fn a_reader_holding_the_file_delays_the_move_but_does_not_lose_it() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("list.tmp"), dir.path().join("list.json"));
        std::fs::write(&to, b"old").unwrap();
        std::fs::write(&from, b"new").unwrap();
        // Share mode 1: others may read, nobody may delete or replace.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&to).unwrap();
        assert!(
            std::fs::rename(&from, &to).is_err(),
            "the hold does not block a plain rename on this system"
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(held);
        });
        replace_file(&from, &to).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
    }
}
