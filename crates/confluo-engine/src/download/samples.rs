//! Small byte samples that show whether two sources, or a source and the bytes
//! already on disk, hold the same file.

use super::*;

pub(super) async fn fetch_bytes(
    bind: Option<&BindSpec>,
    url: &Url,
    opts: &RequestOptions,
    start: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let mut slot = None;
    let ask = Ask { range: Some((start, Some(start + len - 1))), if_range: None };
    let fetched = http::get(&mut slot, bind, url, opts, &ask).await?;
    http::check_status(&fetched.response)?;
    if fetched.response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(Error::Protocol("no range support".into()));
    }
    // One byte more than asked for is enough to tell a wrong answer; reading on would
    // let a server fill memory.
    let limited = http_body_util::Limited::new(fetched.response.into_body(), len as usize + 1);
    let body = tokio::time::timeout(opts.header_timeout, limited.collect())
        .await
        .map_err(|_| Error::Timeout("sample"))?
        .map_err(|_| Error::Protocol("oversized sample".into()))?;
    let bytes = body.to_bytes();
    if bytes.len() as u64 != len {
        return Err(Error::Protocol("short sample".into()));
    }
    Ok(bytes.to_vec())
}

/// Checks that `mirror` serves the same bytes as `primary` at three places.
pub(super) async fn same_content(
    bind: Option<&BindSpec>,
    primary: &Probe,
    mirror: &Probe,
    opts: &RequestOptions,
) -> Result<bool> {
    let Some(size) = primary.size else { return Ok(false) };
    if mirror.size != Some(size) || !mirror.ranges {
        return Ok(false);
    }
    if let (Some(a), Some(b)) = (&primary.sha256, &mirror.sha256) {
        return Ok(a == b);
    }
    let len = SAMPLE_LEN.min(size);
    for start in [0, (size - len) / 2, size - len] {
        let (a, b) = tokio::join!(
            fetch_bytes(bind, &primary.url, opts, start, len),
            fetch_bytes(bind, &mirror.url, opts, start, len)
        );
        if a? != b? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether the server's validators show that a journal's bytes belong to the file it serves now.
pub(super) fn vouched_for(j: &Journal, p: &Probe) -> bool {
    matches!((&j.etag, &p.etag), (Some(_), Some(_)))
        || matches!((&j.last_modified, &p.last_modified), (Some(_), Some(_)))
}

/// Compares bytes already on disk with what the server sends for the same places:
/// the start and the end of the first and the last stretch received. A sample that
/// cannot be fetched proves nothing, so the bytes are kept as before.
pub(super) async fn still_the_same(
    bind: Option<&BindSpec>,
    primary: &Probe,
    opts: &RequestOptions,
    part: &Path,
    done: &RangeSet,
) -> bool {
    let spans = done.spans();
    let (Some(&(a, b)), Some(&(c, d))) = (spans.first(), spans.last()) else { return true };
    let mut places = vec![(a, SAMPLE_LEN.min(b - a)), (d - SAMPLE_LEN.min(d - c), SAMPLE_LEN.min(d - c))];
    places.dedup();
    for (start, len) in places {
        let Ok(theirs) = fetch_bytes(bind, &primary.url, opts, start, len).await else { return true };
        match disk::read_at(part, start, len) {
            Ok(ours) if ours == theirs => {}
            Ok(_) => return false,
            Err(_) => return true,
        }
    }
    true
}
