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
}

impl Default for SchedConfig {
    fn default() -> Self {
        Self {
            min_piece: 256 * 1024,
            max_piece: 64 * 1024 * 1024,
            first_piece: 1024 * 1024,
            target_secs: 3.0,
            min_steal: 512 * 1024,
            min_steal_eta_secs: 1.5,
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
}

impl Scheduler {
    /// `pending` is every byte that still has to be fetched.
    pub fn new(cfg: SchedConfig, pending: RangeSet) -> Self {
        Self { cfg, pending, active: BTreeMap::new(), next_id: 1, steals: 0 }
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

    fn piece_for(&self, rate: f64) -> u64 {
        if rate <= 0.0 {
            return self.cfg.first_piece.clamp(self.cfg.min_piece, self.cfg.max_piece);
        }
        ((rate * self.cfg.target_secs) as u64).clamp(self.cfg.min_piece, self.cfg.max_piece)
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
        self.active.insert(id, Active { pos: pos.clone(), end: end.clone(), rate });
        Some(Lease { id, start, pos, end })
    }

    fn best_victim(&self, _thief_rate: f64) -> Option<u64> {
        self.active
            .iter()
            .filter(|(_, a)| a.remaining() >= 2 * self.cfg.min_steal)
            .map(|(id, a)| (*id, a.remaining() as f64 / a.rate.max(1.0)))
            .filter(|(_, eta)| *eta >= self.cfg.min_steal_eta_secs)
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
        let share = if thief_rate > 0.0 && victim.rate > 0.0 {
            victim.rate / (victim.rate + thief_rate)
        } else {
            0.5
        };
        let keep =
            ((remaining as f64 * share) as u64).clamp(self.cfg.min_steal, remaining - self.cfg.min_steal);
        let split = pos + keep;
        victim.end.store(split, Ordering::Release);
        self.steals += 1;
        Some((split, end))
    }

    /// Updates the speed the scheduler assumes for a lease.
    pub fn set_rate(&mut self, id: u64, rate: f64) {
        if let Some(a) = self.active.get_mut(&id) {
            a.rate = rate;
        }
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
        let mut s = Scheduler::new(cfg(), RangeSet::full(100));
        let a = s.acquire(1000.0).unwrap();
        assert_eq!((a.start, a.end()), (0, 100));
        a.pos.store(20, Ordering::Release);
        let b = s.acquire(0.0).unwrap();
        assert_eq!(a.end(), b.start);
        assert_eq!(b.end(), 100);
        assert!(b.start >= 40 && b.start <= 80);
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
        fn every_byte_once(size in 1u64..5000, ops in prop::collection::vec((0u8..4, 0usize..8, 1u64..400, 0f64..2000.0), 1..400)) {
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
