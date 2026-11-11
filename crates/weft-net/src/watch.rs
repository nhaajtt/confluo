//! Interface change notifications.

use std::time::Duration;

use tokio::sync::watch;

use crate::{Interface, usable_interfaces};

/// Returns a channel that always holds the current list of usable interfaces.
///
/// The list is refreshed when the OS reports an address change (Windows) and
/// by a slow poll everywhere as a safety net. Must be called inside a Tokio runtime.
pub fn watch_changes() -> watch::Receiver<Vec<Interface>> {
    let initial = usable_interfaces().unwrap_or_default();
    let (tx, rx) = watch::channel(initial);
    let wake = crate::sys::change_signal();
    tokio::spawn(async move {
        let poll = if wake.is_some() { Duration::from_secs(5) } else { Duration::from_secs(1) };
        loop {
            match &wake {
                Some(n) => {
                    let _ = tokio::time::timeout(poll, n.notified()).await;
                    // Address changes arrive in bursts; let them settle.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                None => tokio::time::sleep(poll).await,
            }
            if tx.is_closed() {
                break;
            }
            if let Ok(now) = usable_interfaces() {
                tx.send_if_modified(|cur| {
                    if *cur != now {
                        *cur = now;
                        true
                    } else {
                        false
                    }
                });
            }
        }
    });
    rx
}
