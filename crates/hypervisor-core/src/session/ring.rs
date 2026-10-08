//! The output ring: a session's recent PTY output, bounded by bytes.
//!
//! Sequence numbers count bytes. Byte `n` of the session's output, counted from zero, has
//! sequence `n`, so a viewer that has seen everything up to sequence `s` resumes with
//! [`OutputRing::read_from`]`(s)`, whatever the chunk boundaries were. Eviction drops bytes from the
//! front one at a time, so the retained bytes fill the budget exactly once enough has arrived.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

/// What [`OutputRing::read_from`] found at a sequence number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RingRead {
    /// Every byte appended from the requested sequence onward, in order. Empty when the request
    /// was for [`OutputRing::next`].
    Bytes(Vec<u8>),
    /// The requested bytes were evicted. The reader has lost output and needs a fresh snapshot;
    /// `oldest` is the first sequence still retained.
    Gone {
        /// The first retained sequence.
        oldest: u64,
    },
    /// The request was past the end of the output. `next` is the sequence the next byte gets.
    Ahead {
        /// The sequence of the next byte to be appended.
        next: u64,
    },
}

/// A byte-bounded buffer of recent output with monotonically increasing sequence numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputRing {
    budget: NonZeroUsize,
    bytes: VecDeque<u8>,
    /// The sequence of `bytes[0]`.
    oldest: u64,
}

impl OutputRing {
    /// An empty ring that retains at most `budget` bytes.
    #[must_use]
    pub fn new(budget: NonZeroUsize) -> Self {
        Self {
            budget,
            bytes: VecDeque::new(),
            oldest: 0,
        }
    }

    /// The byte budget.
    #[must_use]
    pub const fn budget(&self) -> NonZeroUsize {
        self.budget
    }

    /// The first retained sequence.
    #[must_use]
    pub const fn oldest(&self) -> u64 {
        self.oldest
    }

    /// The sequence the next appended byte gets, which is also the count of bytes ever appended.
    #[must_use]
    pub fn next(&self) -> u64 {
        self.oldest + self.bytes.len() as u64
    }

    /// Bytes currently retained, never more than the budget.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether nothing is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Appends `chunk` and evicts from the front until the budget holds.
    pub fn append(&mut self, chunk: &[u8]) {
        let budget = self.budget.get();
        // A chunk at least as large as the budget replaces everything; only its tail survives.
        let keep = &chunk[chunk.len().saturating_sub(budget)..];
        let skipped = chunk.len() - keep.len();
        let overflow = (self.bytes.len() + keep.len()).saturating_sub(budget);
        self.bytes.drain(..overflow);
        self.oldest += (overflow + skipped) as u64;
        self.bytes.extend(keep);
    }

    /// The bytes appended from `seq` onward, or why they cannot be returned.
    #[must_use]
    pub fn read_from(&self, seq: u64) -> RingRead {
        if seq < self.oldest {
            return RingRead::Gone {
                oldest: self.oldest,
            };
        }
        let next = self.next();
        if seq > next {
            return RingRead::Ahead { next };
        }
        let skip = usize::try_from(seq - self.oldest)
            .unwrap_or_else(|_| unreachable!("the offset is within the retained bytes"));
        RingRead::Bytes(self.bytes.iter().skip(skip).copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputRing, RingRead};
    use proptest::prelude::*;
    use std::num::NonZeroUsize;

    fn ring(budget: usize) -> OutputRing {
        OutputRing::new(NonZeroUsize::new(budget).unwrap())
    }

    #[test]
    fn reads_everything_from_zero_before_eviction() {
        let mut r = ring(8);
        r.append(b"abc");
        r.append(b"de");
        assert_eq!(r.read_from(0), RingRead::Bytes(b"abcde".to_vec()));
        assert_eq!(r.read_from(3), RingRead::Bytes(b"de".to_vec()));
        assert_eq!(r.read_from(5), RingRead::Bytes(Vec::new()));
        assert_eq!(r.read_from(6), RingRead::Ahead { next: 5 });
    }

    #[test]
    fn eviction_answers_gone_with_the_oldest_retained_sequence() {
        let mut r = ring(4);
        r.append(b"abcdef");
        assert_eq!(r.oldest(), 2);
        assert_eq!(r.next(), 6);
        assert_eq!(r.read_from(1), RingRead::Gone { oldest: 2 });
        assert_eq!(r.read_from(2), RingRead::Bytes(b"cdef".to_vec()));
        r.append(b"g");
        assert_eq!(r.read_from(2), RingRead::Gone { oldest: 3 });
        assert_eq!(r.read_from(3), RingRead::Bytes(b"defg".to_vec()));
    }

    #[test]
    fn an_empty_append_changes_nothing() {
        let mut r = ring(2);
        r.append(b"ab");
        let before = r.clone();
        r.append(b"");
        assert_eq!(r, before);
    }

    proptest! {
        #[test]
        fn sequences_never_decrease_and_the_budget_holds(
            budget in 1usize..64,
            chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..80), 0..20),
        ) {
            let mut r = ring(budget);
            let mut appended = 0u64;
            for chunk in &chunks {
                let (oldest, next) = (r.oldest(), r.next());
                r.append(chunk);
                appended += chunk.len() as u64;
                prop_assert!(r.oldest() >= oldest);
                prop_assert!(r.next() >= next);
                prop_assert_eq!(r.next(), appended);
                prop_assert!(r.len() <= budget);
            }
        }

        #[test]
        fn a_read_from_a_retained_sequence_returns_exactly_the_bytes_since(
            budget in 1usize..64,
            chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..80), 0..20),
            pick in any::<proptest::sample::Index>(),
        ) {
            let mut r = ring(budget);
            let mut all = Vec::new();
            for chunk in &chunks {
                r.append(chunk);
                all.extend_from_slice(chunk);
            }
            let span = usize::try_from(r.next() - r.oldest()).unwrap();
            let seq = r.oldest() + pick.index(span + 1) as u64;
            let from = usize::try_from(seq).unwrap();
            prop_assert_eq!(r.read_from(seq), RingRead::Bytes(all[from..].to_vec()));
            if r.oldest() > 0 {
                prop_assert_eq!(r.read_from(r.oldest() - 1), RingRead::Gone { oldest: r.oldest() });
            }
        }
    }
}
