//! A fixed-capacity, allocation-free set of physical keys in deterministic
//! (sorted) order — the state machine's held-key set.
//!
//! Replaces `BTreeSet` in `KeyboardState`: bounded by the hard rollover cap
//! (32), never allocates, and every operation is a bounded linear scan over
//! a fixed array — so the state machine is model-checkable (Kani cannot
//! verify the std BTree's heap-allocating internals). Sorted order and
//! semantics are identical to the previous representation; the public API is
//! unchanged.

use crate::key::PhysicalKey;

/// The hard rollover bound (matches the broker's `MAX_HELD_KEYS_LIMIT`).
pub const MAX_HELD_KEYS: usize = 32;

/// A sorted set of physical keys with a fixed capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySet {
    keys: [Option<PhysicalKey>; MAX_HELD_KEYS],
    len: usize,
}

impl KeySet {
    pub const fn new() -> Self {
        KeySet {
            keys: [None; MAX_HELD_KEYS],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains(&self, key: PhysicalKey) -> bool {
        // Early-return-free loop: CBMC's loop-bound analysis derives the
        // exact trip bound (MAX_HELD_KEYS) only when the loop has no early
        // return — otherwise it falls back to the unwind cap. The invariant
        // `len <= MAX_HELD_KEYS` is not visible to the solver, so the scan
        // covers the full fixed array (slots at/after `len` are `None`).
        let mut found = false;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if self.keys[i] == Some(key) {
                found = true;
            }
            i += 1;
        }
        found
    }

    /// How many times `key` appears in the set — 0 or 1 by construction.
    ///
    /// The explicit bounded scan (rather than `iter().filter().count()`, a
    /// fused std iterator chain) keeps the query allocation-free and makes
    /// the loop trivially model-checkable.
    pub fn count_of(&self, key: PhysicalKey) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if self.keys[i] == Some(key) {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Insert keeping the array sorted. A duplicate is a no-op; at capacity
    /// (the hard rollover bound) the insert is dropped — the state machine's
    /// rollover guard prevents reaching capacity, and the set must never
    /// panic.
    pub fn insert(&mut self, key: PhysicalKey) {
        if self.len >= MAX_HELD_KEYS || self.contains(key) {
            return;
        }
        // Sorted position: the first slot holding a key greater than `key`.
        // (Slots at/after `len` are `None` and fail the comparison, so the
        // constant-bound scan is equivalent to scanning `0..len`.)
        let mut pos = self.len;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if self.keys[i].is_some_and(|k| k > key) {
                pos = i;
                break;
            }
            i += 1;
        }
        // Shift `[pos, len)` one slot right, descending (constant bound).
        let mut k = 0;
        while k < MAX_HELD_KEYS {
            let j = MAX_HELD_KEYS - 1 - k;
            if j > pos && j <= self.len {
                self.keys[j] = self.keys[j - 1];
            }
            k += 1;
        }
        self.keys[pos] = Some(key);
        self.len += 1;
    }

    /// Remove `key`; returns whether it was present.
    pub fn remove(&mut self, key: PhysicalKey) -> bool {
        let mut pos = None;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if self.keys[i] == Some(key) {
                pos = Some(i);
                break;
            }
            i += 1;
        }
        let Some(pos) = pos else { return false };
        // Shift `[pos, len)` one slot left, ascending (constant bound).
        let mut k = 0;
        while k < MAX_HELD_KEYS {
            let j = k;
            if j >= pos && j + 1 < self.len {
                self.keys[j] = self.keys[j + 1];
            }
            k += 1;
        }
        self.keys[self.len - 1] = None;
        self.len -= 1;
        true
    }

    pub fn clear(&mut self) {
        self.keys = [None; MAX_HELD_KEYS];
        self.len = 0;
    }

    /// Ascending (sorted) iteration.
    ///
    /// A concrete, index-based iterator: no std adapter fusion (fused
    /// iterator chains such as `flatten`/`Fuse` are un-bounded to CBMC and
    /// make model checking spiral — see the Kani proof notes).
    pub fn iter(&self) -> KeyIter<'_> {
        KeyIter {
            slots: &self.keys,
            front: 0,
            back: self.len,
        }
    }

    /// Descending iteration (sorted order, reversed).
    pub fn iter_rev(&self) -> std::iter::Rev<KeyIter<'_>> {
        self.iter().rev()
    }

    /// Copy the present keys (sorted) into `buf`, returning the count.
    ///
    /// One flat, constant-bound loop with no early return: CBMC derives the
    /// exact trip bound. This is the model-checker-friendly scan primitive —
    /// callers then index-loop over the bounded snapshot instead of driving
    /// the iterator, which would nest `KeyIter::next`'s unrolling inside
    /// every consuming loop (a solver-state explosion).
    pub fn copy_into(&self, buf: &mut [PhysicalKey; MAX_HELD_KEYS]) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some(k) = self.keys[i] {
                buf[n] = k;
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Whether any key appears more than once (always false by construction
    /// — the uniqueness invariant the state machine preserves).
    ///
    /// The set is sorted by construction, so any duplicate must be adjacent:
    /// a single constant-bound pass over neighbour pairs is exactly
    /// equivalent to a full pairwise scan and stays cheap for the solver.
    pub fn has_duplicates(&self) -> bool {
        let mut dup = false;
        let mut i = 0;
        while i + 1 < MAX_HELD_KEYS {
            if self.keys[i] != None && self.keys[i] == self.keys[i + 1] {
                dup = true;
            }
            i += 1;
        }
        dup
    }
}

/// The concrete iterator over a [`KeySet`]'s present keys.
#[derive(Debug, Clone)]
pub struct KeyIter<'a> {
    slots: &'a [Option<PhysicalKey>; MAX_HELD_KEYS],
    front: usize,
    back: usize,
}

impl<'a> Iterator for KeyIter<'a> {
    type Item = PhysicalKey;

    fn next(&mut self) -> Option<PhysicalKey> {
        // Purely constant trip bound: CBMC derives the exact unroll (exactly
        // MAX_HELD_KEYS) and the loop exits deterministically — one path per
        // call. The fused end-cursor check (`front >= back`) is an ordinary
        // branch, NOT part of the loop guard: a symbolic guard would make
        // CBMC explore an exit point at every unroll.
        while self.front < MAX_HELD_KEYS {
            if self.front >= self.back {
                return None;
            }
            let slot = self.slots[self.front];
            self.front += 1;
            if let Some(k) = slot {
                return Some(k);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.back.saturating_sub(self.front)))
    }
}

impl<'a> DoubleEndedIterator for KeyIter<'a> {
    fn next_back(&mut self) -> Option<PhysicalKey> {
        // Symmetric: `back` decrements from at most `len <= MAX_HELD_KEYS`
        // down to 0, so the trip bound is exactly the fixed capacity.
        while self.back > 0 {
            if self.back <= self.front {
                return None;
            }
            self.back -= 1;
            let slot = self.slots[self.back];
            if let Some(k) = slot {
                return Some(k);
            }
        }
        None
    }
}

impl Default for KeySet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_keeps_sorted_order() {
        let mut s = KeySet::new();
        s.insert(PhysicalKey::C);
        s.insert(PhysicalKey::A);
        s.insert(PhysicalKey::B);
        let got: Vec<_> = s.iter().collect();
        let mut sorted = got.clone();
        sorted.sort();
        assert_eq!(got, sorted);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn duplicate_insert_is_a_noop() {
        let mut s = KeySet::new();
        s.insert(PhysicalKey::A);
        s.insert(PhysicalKey::A);
        assert_eq!(s.len(), 1);
        assert_eq!(s.iter().count(), 1);
    }

    #[test]
    fn remove_shifts_and_clears() {
        let mut s = KeySet::new();
        s.insert(PhysicalKey::A);
        s.insert(PhysicalKey::B);
        s.insert(PhysicalKey::C);
        assert!(s.remove(PhysicalKey::B));
        assert!(!s.remove(PhysicalKey::B));
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.iter().collect::<Vec<_>>(),
            vec![PhysicalKey::A, PhysicalKey::C]
        );
        assert_eq!(
            s.iter_rev().collect::<Vec<_>>(),
            vec![PhysicalKey::C, PhysicalKey::A]
        );
    }

    #[test]
    fn clear_resets() {
        let mut s = KeySet::new();
        s.insert(PhysicalKey::A);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn respects_the_hard_cap() {
        // The rollover guard in the state machine prevents overflow; the set
        // itself must not silently exceed its capacity.
        let mut s = KeySet::new();
        for code in 0..(MAX_HELD_KEYS as u32 + 8) {
            let _ = s.insert(PhysicalKey::from_linux_code(code).unwrap_or(PhysicalKey::Escape));
        }
        assert!(s.len() <= MAX_HELD_KEYS);
    }
}
