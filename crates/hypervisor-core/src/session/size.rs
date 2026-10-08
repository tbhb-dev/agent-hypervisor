//! Terminal size as last-size-wins state (RFC-36 proposal lines 88 and 89).
//!
//! Requests are not queued as events. The holder records the latest request and settles it once
//! per batch of inputs, so a burst of requests applies only its last size. A request for the size
//! already applied sends no resize, because macOS and Linux send no `SIGWINCH` for a same-size
//! `TIOCSWINSZ` (RFC-40 run 11); it asks for a redraw hint instead, as zmx does (RFC-36 run 4).

use crate::emulator::Size;

/// What settling the requests of one batch decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// No request arrived.
    Idle,
    /// Apply this size to the PTY and the emulator.
    Apply(Size),
    /// The last request named the size already applied: send a redraw hint, not a resize.
    Unchanged,
}

/// The applied size and the latest request not yet settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeState {
    applied: Size,
    requested: Option<Size>,
}

impl SizeState {
    /// State with `initial` applied, as the PTY was spawned.
    #[must_use]
    pub const fn new(initial: Size) -> Self {
        Self {
            applied: initial,
            requested: None,
        }
    }

    /// The size the PTY and emulator have.
    #[must_use]
    pub const fn applied(&self) -> Size {
        self.applied
    }

    /// Records a request; a later request replaces it.
    pub fn request(&mut self, size: Size) {
        self.requested = Some(size);
    }

    /// Settles the latest request and forgets it.
    pub fn settle(&mut self) -> Settled {
        match self.requested.take() {
            None => Settled::Idle,
            Some(size) if size == self.applied => Settled::Unchanged,
            Some(size) => {
                self.applied = size;
                Settled::Apply(size)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Settled, SizeState};
    use crate::emulator::Size;
    use proptest::prelude::*;

    fn size(cols: u16, rows: u16) -> Size {
        Size::new(cols, rows).unwrap()
    }

    #[test]
    fn a_same_size_request_is_unchanged() {
        let mut s = SizeState::new(size(80, 24));
        s.request(size(80, 24));
        assert_eq!(s.settle(), Settled::Unchanged);
        assert_eq!(s.settle(), Settled::Idle);
    }

    #[test]
    fn a_burst_applies_only_its_last_size() {
        let mut s = SizeState::new(size(80, 24));
        s.request(size(100, 30));
        s.request(size(120, 40));
        assert_eq!(s.settle(), Settled::Apply(size(120, 40)));
        assert_eq!(s.applied(), size(120, 40));
    }

    #[test]
    fn a_burst_back_to_the_applied_size_is_unchanged() {
        let mut s = SizeState::new(size(80, 24));
        s.request(size(100, 30));
        s.request(size(80, 24));
        assert_eq!(s.settle(), Settled::Unchanged);
    }

    proptest! {
        #[test]
        fn settling_a_burst_is_decided_by_its_last_request(
            initial in (1u16..300, 1u16..100),
            burst in proptest::collection::vec((1u16..300, 1u16..100), 1..10),
        ) {
            let initial = size(initial.0, initial.1);
            let mut s = SizeState::new(initial);
            for &(c, r) in &burst {
                s.request(size(c, r));
            }
            let (c, r) = burst[burst.len() - 1];
            let last = size(c, r);
            let expected = if last == initial { Settled::Unchanged } else { Settled::Apply(last) };
            prop_assert_eq!(s.settle(), expected);
            prop_assert_eq!(s.applied(), last);
        }
    }
}
