//! Which partial file a download writes to, and keeping two downloads off the same one.

use super::*;

pub(super) struct Paths {
    pub(super) name: String,
    pub(super) part: PathBuf,
    pub(super) journal: PathBuf,
    /// Keeps other downloads in this process away from `part` until this one ends.
    _claim: Claim,
}

/// Partial files being written by downloads in this process. A second download of
/// the same link must neither adopt nor overwrite one of them.
pub(super) static IN_USE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub(super) struct Claim(PathBuf);

impl Drop for Claim {
    fn drop(&mut self) {
        IN_USE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|p| p != &self.0);
    }
}

/// Picks the file to write to: an interrupted download of the same link if there
/// is one, otherwise a name that collides with nothing.
pub(super) fn plan_paths(spec: &Spec, p: &Probe) -> (Paths, Option<Journal>) {
    let mut busy = IN_USE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let base = name::sanitize(spec.file_name.as_deref().or(p.file_name.as_deref()).unwrap_or("download"));
    let part = name::part_path(&spec.dir, &base);
    let jpath = journal::journal_path(&part);
    if !busy.contains(&part)
        && let Some(j) = journal::load(&jpath)
    {
        let same_link = match (j.urls.first(), spec.urls.first()) {
            (Some(recorded), Some(url)) => journal::same_link(recorded, url.as_str()),
            _ => false,
        };
        let same_etag = match (&j.etag, &p.etag) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let same_date = match (&j.last_modified, &p.last_modified) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        let part_ok = std::fs::metadata(&part).is_ok_and(|m| p.size.is_none_or(|s| m.len() == s));
        if same_link && j.size == p.size && p.size.is_some() && p.ranges && same_etag && same_date && part_ok
        {
            busy.push(part.clone());
            let claim = Claim(part.clone());
            return (Paths { name: base, part, journal: jpath, _claim: claim }, Some(j));
        }
    }
    let name = name::unique(&spec.dir, &base, |f| {
        f.exists()
            || f.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                let part = name::part_path(&spec.dir, n);
                part.exists() || busy.contains(&part)
            })
    });
    let part = name::part_path(&spec.dir, &name);
    let journal = journal::journal_path(&part);
    busy.push(part.clone());
    let claim = Claim(part.clone());
    (Paths { name, part, journal, _claim: claim }, None)
}
