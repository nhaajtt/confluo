//! A set of byte ranges, kept as sorted, disjoint, non-adjacent half-open intervals.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RangeSet {
    spans: Vec<(u64, u64)>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// The set covering `[0, size)`.
    pub fn full(size: u64) -> Self {
        let mut s = Self::new();
        s.insert(0, size);
        s
    }

    /// Builds a set from untrusted spans (for example a journal on disk).
    pub fn from_spans(spans: impl IntoIterator<Item = (u64, u64)>) -> Self {
        let mut s = Self::new();
        for (a, b) in spans {
            s.insert(a, b);
        }
        s
    }

    pub fn spans(&self) -> &[(u64, u64)] {
        &self.spans
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Total number of bytes in the set.
    pub fn total(&self) -> u64 {
        self.spans.iter().map(|(a, b)| b - a).sum()
    }

    /// Adds `[start, end)`, merging with neighbours.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        // First span that ends at or after `start` (touching counts, so it merges).
        let lo = self.spans.partition_point(|&(_, b)| b < start);
        // First span that starts after `end`.
        let hi = self.spans.partition_point(|&(a, _)| a <= end);
        if lo == hi {
            self.spans.insert(lo, (start, end));
            return;
        }
        let merged = (start.min(self.spans[lo].0), end.max(self.spans[hi - 1].1));
        self.spans.splice(lo..hi, [merged]);
    }

    /// Removes `[start, end)`.
    pub fn remove(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let lo = self.spans.partition_point(|&(_, b)| b <= start);
        let hi = self.spans.partition_point(|&(a, _)| a < end);
        if lo >= hi {
            return;
        }
        let mut keep = Vec::with_capacity(2);
        if self.spans[lo].0 < start {
            keep.push((self.spans[lo].0, start));
        }
        if self.spans[hi - 1].1 > end {
            keep.push((end, self.spans[hi - 1].1));
        }
        self.spans.splice(lo..hi, keep);
    }

    /// Whether every byte of `[start, end)` is in the set.
    pub fn contains(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }
        let i = self.spans.partition_point(|&(_, b)| b <= start);
        self.spans.get(i).is_some_and(|&(a, b)| a <= start && end <= b)
    }

    /// Everything in `[0, size)` that is not in the set.
    pub fn complement(&self, size: u64) -> Self {
        let mut out = Vec::new();
        let mut at = 0;
        for &(a, b) in &self.spans {
            if a >= size {
                break;
            }
            if a > at {
                out.push((at, a));
            }
            at = at.max(b);
        }
        if at < size {
            out.push((at, size));
        }
        Self { spans: out }
    }

    /// Removes and returns up to `len` bytes from the front of the lowest span.
    /// A remainder smaller than `min_left` is taken along instead of being left behind.
    pub fn take_front(&mut self, len: u64, min_left: u64) -> Option<(u64, u64)> {
        let &(a, b) = self.spans.first()?;
        let mut end = a.saturating_add(len.max(1)).min(b);
        if b - end < min_left {
            end = b;
        }
        if end == b {
            self.spans.remove(0);
        } else {
            self.spans[0].0 = end;
        }
        Some((a, end))
    }

    /// Like [`Self::take_front`], but from the span that starts exactly at `start`.
    /// Returns the end of what was taken.
    pub fn take_at(&mut self, start: u64, len: u64, min_left: u64) -> Option<u64> {
        let i = self.spans.partition_point(|&(a, _)| a < start);
        let &(a, b) = self.spans.get(i)?;
        if a != start {
            return None;
        }
        let mut end = a.saturating_add(len.max(1)).min(b);
        if b - end < min_left {
            end = b;
        }
        if end == b {
            self.spans.remove(i);
        } else {
            self.spans[i].0 = end;
        }
        Some(end)
    }

    /// Fraction of each of `cells` equal slices of `[0, size)` that is in the set, as 0..=255.
    pub fn density(&self, size: u64, cells: usize) -> Vec<u8> {
        let mut out = vec![0u8; cells];
        if size == 0 || cells == 0 {
            return out;
        }
        let cell = (size as f64 / cells as f64).max(1.0);
        let mut acc = vec![0f64; cells];
        for &(a, b) in &self.spans {
            let b = b.min(size);
            if a >= b {
                continue;
            }
            let first = ((a as f64 / cell) as usize).min(cells - 1);
            let last = (((b - 1) as f64 / cell) as usize).min(cells - 1);
            for (i, slot) in acc.iter_mut().enumerate().take(last + 1).skip(first) {
                let lo = (i as f64 * cell).max(a as f64);
                let hi = ((i + 1) as f64 * cell).min(b as f64);
                *slot += (hi - lo).max(0.0);
            }
        }
        for (o, a) in out.iter_mut().zip(acc) {
            *o = ((a / cell).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn insert_merges() {
        let mut s = RangeSet::new();
        s.insert(10, 20);
        s.insert(30, 40);
        s.insert(20, 30);
        assert_eq!(s.spans(), &[(10, 40)]);
        s.insert(0, 5);
        s.insert(4, 11);
        assert_eq!(s.spans(), &[(0, 40)]);
    }

    #[test]
    fn remove_splits() {
        let mut s = RangeSet::full(100);
        s.remove(10, 20);
        assert_eq!(s.spans(), &[(0, 10), (20, 100)]);
        s.remove(0, 100);
        assert!(s.is_empty());
    }

    #[test]
    fn take_front_avoids_crumbs() {
        let mut s = RangeSet::full(100);
        assert_eq!(s.take_front(95, 10), Some((0, 100)));
        let mut s = RangeSet::full(100);
        assert_eq!(s.take_front(50, 10), Some((0, 50)));
        assert_eq!(s.spans(), &[(50, 100)]);
    }

    proptest! {
        /// The set behaves exactly like a bitmap under any mix of inserts and removes.
        #[test]
        fn matches_bitmap(ops in prop::collection::vec((any::<bool>(), 0u64..200, 0u64..200), 0..60)) {
            let mut set = RangeSet::new();
            let mut bits = [false; 200];
            for (ins, a, b) in ops {
                let (a, b) = (a.min(b), a.max(b));
                if ins { set.insert(a, b) } else { set.remove(a, b) }
                for bit in &mut bits[a as usize..b as usize] { *bit = ins; }
            }
            let expect: u64 = bits.iter().filter(|b| **b).count() as u64;
            prop_assert_eq!(set.total(), expect);
            for w in set.spans().windows(2) { prop_assert!(w[0].1 < w[1].0); }
            for &(a, b) in set.spans() {
                prop_assert!(a < b);
                prop_assert!(bits[a as usize..b as usize].iter().all(|x| *x));
            }
            let comp = set.complement(200);
            prop_assert_eq!(comp.total() + set.total(), 200);
        }
    }
}
