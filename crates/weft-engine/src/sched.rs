//! Decides which bytes each connection fetches next.
//!
//! The scheduler has no I/O and no clock, so it can be tested exhaustively.
//!
//! * A connection asks for work and gets a *lease*: a byte range sized so that
//!   it lasts about `target_secs` at the speed that connection has shown. Fast
//!   links therefore take big ranges and slow links small ones, and a slow link
//!   never sits on a large part of the file.
//! * When nothing is left unassigned, an idle connection *steals* the tail of
//!   the lease that would take longest to finish. The victim keeps the front
//!   and stops at the split point, so no byte is downloaded twice.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ranges::RangeSet;

#[derive(Debug, Clone)]
pub struct SchedConfig {
    /// Smallest range worth a request.
    pub min_piece: u64,
    /// Largest range handed out at once.
    pub max_piece: u64,
    /// Range size before a connection has a measured speed.
    pub first_piece: u64,
    /// How long one range should take at the connection's speed.
    pub target_secs: f64,
    /// A lease is only split when both halves are at least this big.
    pub min_steal: u64,
    /// A lease is only split when its holder needs at least this long to finish it.
    pub min_steal_eta_secs: f64,
    /// Hand each connection an equal share of what is left instead of a time-sized
    /// range. A share is one long uninterrupted response, which matters on long
    /// paths: every new request restarts TCP slow start on the server.
    pub equal_split: bool,
}

impl Default for SchedConfig {
    fn default() -> Self {
        Self {
            // Every range costs the server a request, and some servers count requests.
            // Ranges are therefore long: a 100 MiB file takes around a dozen requests.
            min_piece: 1024 * 1024,
            max_piece: 1024 * 1024 * 1024,
            first_piece: 4 * 1024 * 1024,
            target_secs: 8.0,
            min_steal: 512 * 1024,
            min_steal_eta_secs: 1.5,
            equal_split: true,
        }
    }
}

/// A byte range one connection is responsible for.
///
/// `pos` is advanced by the holder as bytes are handed to the disk writer.
/// `end` can be lowered by the scheduler when another connection steals the tail;
/// the holder must re-read it before every write.
#[derive(Debug, Clone)]
pub struct Lease {
    pub id: u64,
    pub start: u64,
    pub pos: Arc<AtomicU64>,
    pub end: Arc<AtomicU64>,
}

impl Lease {
    pub fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }

    pub fn pos(&self) -> u64 {
        self.pos.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct Active {
    pos: Arc<AtomicU64>,
    end: Arc<AtomicU64>,
    rate: f64,
    /// False until the holder has reported a speed. A lease that has not delivered
    /// anything yet is not slow, it is new, so nobody takes its tail.
    measured: bool,
}

impl Active {
    fn remaining(&self) -> u64 {
        self.end.load(Ordering::Acquire).saturating_sub(self.pos.load(Ordering::Acquire))
    }
}

#[derive(Debug)]
pub struct Scheduler {
    cfg: SchedConfig,
    pending: RangeSet,
    active: BTreeMap<u64, Active>,
    next_id: u64,
    steals: u64,
    /// Connections expected to work on this download at the same time.
    parallel: usize,
}

impl Scheduler {
    /// `pending` is every byte that still has to be fetched.
    pub fn new(cfg: SchedConfig, pending: RangeSet) -> Self {
        Self { cfg, pending, active: BTreeMap::new(), next_id: 1, steals: 0, parallel: 1 }
    }

    /// Bytes not yet assigned to any connection.
    pub fn unassigned(&self) -> u64 {
        self.pending.total()
    }

    /// Bytes assigned but not yet fetched.
    pub fn in_flight(&self) -> u64 {
        self.active.values().map(Active::remaining).sum()
    }

    pub fn active_leases(&self) -> usize {
        self.active.len()
    }

    pub fn steals(&self) -> u64 {
        self.steals
    }

    /// True when no byte is pending and no lease is open.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.active.is_empty()
    }

    /// Whether a connection asking now would get work.
    pub fn has_work(&self) -> bool {
        !self.pending.is_empty() || self.best_victim(0.0).is_some()
    }

    /// Tells the scheduler how many connections the download is aiming for.
    pub fn set_parallel(&mut self, n: usize) {
        self.parallel = n.max(1);
    }

    fn piece_for(&self, rate: f64) -> u64 {
        let timed = if rate <= 0.0 { self.cfg.first_piece } else { (rate * self.cfg.target_secs) as u64 };
        let share = if self.cfg.equal_split {
            let waiting = self.parallel.saturating_sub(self.active.len()).max(1) as u64;
            self.pending.total() / waiting
        } else {
            0
        };
        timed.max(share).min(self.cfg.max_piece).max(self.cfg.min_piece)
    }

    /// Gives the caller a range to fetch. `rate` is the caller's measured speed in bytes/s
    /// (0 when unknown).
    pub fn acquire(&mut self, rate: f64) -> Option<Lease> {
        let (start, end) = match self.pending.take_front(self.piece_for(rate), self.cfg.min_piece) {
            Some(r) => r,
            None => self.steal(rate)?,
        };
        let id = self.next_id;
        self.next_id += 1;
        let pos = Arc::new(AtomicU64::new(start));
        let end = Arc::new(AtomicU64::new(end));
        self.active.insert(id, Active { pos: pos.clone(), end: end.clone(), rate, measured: rate > 0.0 });
        Some(Lease { id, start, pos, end })
    }

    fn best_victim(&self, thief_rate: f64) -> Option<u64> {
        // A thief without a measured speed is a connection still starting up. It
        // needs more time to become useful, so the bar for splitting is higher.
        let min_eta =
            if thief_rate > 0.0 { self.cfg.min_steal_eta_secs } else { self.cfg.min_steal_eta_secs * 2.0 };
        self.active
            .iter()
            .filter(|(_, a)| a.measured && a.remaining() >= 2 * self.cfg.min_steal.max(1))
            .map(|(id, a)| (*id, a.remaining() as f64 / a.rate.max(1.0)))
            .filter(|(_, eta)| *eta >= min_eta)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    fn steal(&mut self, thief_rate: f64) -> Option<(u64, u64)> {
        let id = self.best_victim(thief_rate)?;
        let victim = self.active.get(&id)?;
        let pos = victim.pos.load(Ordering::Acquire);
        let end = victim.end.load(Ordering::Acquire);
        let remaining = end.saturating_sub(pos);
        // The victim keeps a share proportional to its speed, so both finish together.
        // A thief that has not shown a speed yet is assumed to manage half the victim's.
        let share = if thief_rate > 0.0 && victim.rate > 0.0 {
            victim.rate / (victim.rate + thief_rate)
        } else {
            2.0 / 3.0
        };
        let keep =
            ((remaining as f64 * share) as u64).clamp(self.cfg.min_steal, remaining - self.cfg.min_steal);
        let split = pos + keep;
        victim.end.store(split, Ordering::Release);
        self.steals += 1;
        Some((split, end))
    }

    /// Switches to as few requests as possible: every connection takes the whole next
    /// unassigned stretch and keeps it until it is done or another connection takes
    /// over its tail. Used when a server limits the number of requests.
    pub fn prefer_few_requests(&mut self) {
        self.cfg.first_piece = u64::MAX;
        self.cfg.min_piece = u64::MAX;
        self.cfg.max_piece = u64::MAX;
        self.cfg.min_steal = self.cfg.min_steal.max(8 * 1024 * 1024);
        self.cfg.min_steal_eta_secs = self.cfg.min_steal_eta_secs.max(10.0);
    }

    /// Updates the speed the scheduler assumes for a lease.
    pub fn set_rate(&mut self, id: u64, rate: f64) {
        if let Some(a) = self.active.get_mut(&id) {
            a.rate = rate;
            a.measured = true;
        }
    }

    /// Grows a lease into the unassigned bytes right behind it, if there are any.
    /// The holder can then keep reading the same response instead of asking again.
    pub fn extend(&mut self, id: u64) -> bool {
        let Some(a) = self.active.get(&id) else { return false };
        let end = a.end.load(Ordering::Acquire);
        let piece = self.piece_for(a.rate);
        let Some(new_end) = self.pending.take_at(end, piece, self.cfg.min_piece) else { return false };
        self.active[&id].end.store(new_end, Ordering::Release);
        true
    }

    /// Closes a lease. Whatever lies between its position and its end goes back to the pool.
    pub fn release(&mut self, id: u64) {
        if let Some(a) = self.active.remove(&id) {
            let pos = a.pos.load(Ordering::Acquire);
            let end = a.end.load(Ordering::Acquire);
            self.pending.insert(pos, end);
        }
    }

    /// Puts bytes back in the pool (for example after a write was found to be bad).
    pub fn requeue(&mut self, start: u64, end: u64) {
        self.pending.insert(start, end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cfg() -> SchedConfig {
        SchedConfig {
            min_piece: 10,
            max_piece: 1000,
            first_piece: 50,
            target_secs: 1.0,
            min_steal: 20,
            min_steal_eta_secs: 0.0,
            equal_split: false,
        }
    }

    #[test]
    fn sizes_follow_rate() {
        let mut s = Scheduler::new(cfg(), RangeSet::full(10_000));
        let slow = s.acquire(20.0).unwrap();
        let fast = s.acquire(800.0).unwrap();
        let unknown = s.acquire(0.0).unwrap();
        assert_eq!(slow.end() - slow.start, 20);
        assert_eq!(fast.end() - fast.start, 800);
        assert_eq!(unknown.end() - unknown.start, 50);
    }

    #[test]
    fn steal_takes_tail_and_shrinks_victim() {
        let mut s = Scheduler::new(cfg(), RangeSet::full(50));
        let a = s.acquire(0.0).unwrap();
        assert_eq!((a.start, a.end()), (0, 50));
        a.pos.store(10, Ordering::Release);
        // A lease that has not reported a speed yet is new, not slow.
        assert!(s.acquire(0.0).is_none());
        s.set_rate(a.id, 1000.0);
        let b = s.acquire(0.0).unwrap();
        assert_eq!(a.end(), b.start);
        // The newcomer has no measured speed, so the holder keeps two thirds of what is left.
        assert_eq!((b.start, b.end()), (30, 50));
        assert_eq!(s.steals(), 1);
    }

    #[test]
    fn release_returns_unfetched_bytes() {
        let mut s = Scheduler::new(cfg(), RangeSet::full(100));
        let a = s.acquire(1000.0).unwrap();
        a.pos.store(30, Ordering::Release);
        s.release(a.id);
        let b = s.acquire(1000.0).unwrap();
        assert_eq!((b.start, b.end()), (30, 100));
    }

    proptest! {
        /// Under any interleaving of acquire / progress / fail / finish, every byte is
        /// fetched exactly once and the scheduler ends idle.
        #[test]
        fn every_byte_once(size in 1u64..5000, ops in prop::collection::vec((0u8..5, 0usize..8, 1u64..400, 0f64..2000.0), 1..400)) {
            let mut s = Scheduler::new(cfg(), RangeSet::full(size));
            let mut leases: Vec<Lease> = Vec::new();
            let mut fetched = vec![0u8; size as usize];
            let advance = |l: &Lease, n: u64, fetched: &mut Vec<u8>| {
                let pos = l.pos();
                let to = (pos + n).min(l.end());
                for b in &mut fetched[pos as usize..to.max(pos) as usize] { *b += 1; }
                if to > pos { l.pos.store(to, Ordering::Release); }
            };
            for (op, idx, n, rate) in ops {
                match op {
                    0 => { if let Some(l) = s.acquire(rate) { leases.push(l); } }
                    1 if !leases.is_empty() => { let l = &leases[idx % leases.len()]; advance(l, n, &mut fetched); }
                    2 if !leases.is_empty() => { let l = leases.swap_remove(idx % leases.len()); s.release(l.id); }
                    3 if !leases.is_empty() => { let l = &leases[idx % leases.len()]; if l.pos() >= l.end() { s.extend(l.id); } }
                    _ if !leases.is_empty() => {
                        let i = idx % leases.len();
                        if leases[i].pos() >= leases[i].end() { let l = leases.swap_remove(i); s.release(l.id); }
                    }
                    _ => {}
                }
                let held: u64 = leases.iter().map(|l| l.end().saturating_sub(l.pos())).sum();
                let done = fetched.iter().filter(|b| **b > 0).count() as u64;
                prop_assert_eq!(s.unassigned() + held + done, size);
            }
            // Drain: finish everything with plain sequential work.
            for l in leases.drain(..) { advance(&l, u64::MAX / 2, &mut fetched); s.release(l.id); }
            while let Some(l) = s.acquire(100.0) { advance(&l, u64::MAX / 2, &mut fetched); s.release(l.id); }
            prop_assert!(s.is_idle());
            prop_assert!(fetched.iter().all(|b| *b == 1));
        }
    }
}
