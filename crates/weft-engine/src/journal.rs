//! The resume record kept next to a partial file.
//!
//! It is rewritten atomically (temp file, fsync, rename) and only ever lists
//! bytes that were flushed to disk first, so after a crash or power loss every
//! byte it claims is really there.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ranges::RangeSet;

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Journal {
    pub version: u32,
    /// The links the user gave (first is the primary, the rest are mirrors).
    pub urls: Vec<String>,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Final file name inside the destination folder.
    pub file_name: String,
    pub done: RangeSet,
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
    fs::rename(&tmp, path)
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

pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.weft-journal");
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
}
