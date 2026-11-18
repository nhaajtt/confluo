//! The resume record kept next to a partial file.
//!
//! It is rewritten atomically (temp file, fsync, rename) and only ever lists
//! bytes that were flushed to disk first, so after a crash or power loss every
//! byte it claims is really there.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ranges::RangeSet;

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Journal {
    pub version: u32,
    /// The links the user gave (first is the primary, the rest are mirrors), each
    /// written as its [`link_key`]. Journals from before 0.2 hold the links in full.
    pub urls: Vec<String>,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Final file name inside the destination folder.
    pub file_name: String,
    pub done: RangeSet,
}

/// How a link is written into a journal. The journal sits in the download folder,
/// which is often synced or shared, so it holds a digest of the link instead of a
/// signed address or a password; a digest is all that telling links apart needs.
pub fn link_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Whether a link recorded in a journal, as a digest or (older journals) in full, is `url`.
pub fn same_link(recorded: &str, url: &str) -> bool {
    recorded == url || recorded == link_key(url)
}

pub fn journal_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push("-journal");
    PathBuf::from(s)
}

/// Writes the journal so that either the old or the new version survives a crash.
pub fn save(path: &Path, journal: &Journal) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let bytes = serde_json::to_vec(journal).map_err(io::Error::other)?;
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    crate::disk::replace_file(&tmp, path)
}

/// Loads a journal. A missing, unreadable or foreign-version file counts as "no journal".
pub fn load(path: &Path) -> Option<Journal> {
    let bytes = fs::read(path).ok()?;
    let j: Journal = serde_json::from_slice(&bytes).ok()?;
    if j.version != VERSION {
        return None;
    }
    // Never trust spans from disk to be normalised or in range.
    let mut done = RangeSet::from_spans(j.done.spans().iter().copied());
    if let Some(size) = j.size {
        done.remove(size, u64::MAX);
    }
    Some(Journal { done, ..j })
}

/// Whether `dir` holds an interrupted download of `url`.
pub fn exists_for(dir: &Path, url: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".confluo-journal")))
        .filter_map(|p| load(&p))
        .any(|j| j.urls.first().is_some_and(|r| same_link(r, url)) && !j.done.is_empty())
}

pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.confluo-journal");
        let mut j = Journal {
            version: VERSION,
            urls: vec!["http://a/b".into()],
            size: Some(100),
            etag: Some("\"e\"".into()),
            last_modified: None,
            file_name: "b".into(),
            done: RangeSet::from_spans([(0, 10), (50, 60)]),
        };
        save(&p, &j).unwrap();
        assert_eq!(load(&p).unwrap(), j);
        // A journal claiming bytes past the end is clamped, not trusted.
        fs::write(
            &p,
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "urls": ["http://a/b"], "size": 100, "etag": null, "last_modified": null,
                "file_name": "b", "done": [[90, 500], [0, 10], [5, 20]]
            }))
            .unwrap(),
        )
        .unwrap();
        j.done = RangeSet::from_spans([(0, 20), (90, 100)]);
        j.etag = None;
        assert_eq!(load(&p).unwrap(), j);
        fs::write(&p, b"{ not json").unwrap();
        assert!(load(&p).is_none());
    }

    #[test]
    fn links_are_recognised_by_digest_and_in_full() {
        let url = "https://user:pass@example.com/f.zip?sig=abc";
        let key = link_key(url);
        assert!(key.starts_with("sha256:") && !key.contains("pass") && !key.contains("sig"));
        assert!(same_link(&key, url));
        // A journal written before links were stored as digests still matches.
        assert!(same_link(url, url));
        assert!(!same_link(&key, "https://example.com/f.zip"));
        assert!(!same_link("https://example.com/f.zip", url));
    }
}
