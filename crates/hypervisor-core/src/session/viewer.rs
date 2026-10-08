//! Viewer ownership and bounded output delivery for one session.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::time::Duration;

use crate::emulator::{Size, ViewerInputFilter};

/// An opaque identity supplied by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewerId(pub u64);

/// A programmatic source is subject to the same lock as a viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Writer {
    /// An attached viewer.
    Viewer(ViewerId),
    /// An input source such as a prompt or send-keys operation.
    Program(u64),
}

/// The viewer's current mode. Taking the lock promotes `ReadOnly` to `ReadWrite`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerMode {
    /// May receive output only.
    ReadOnly,
    /// May take the lock explicitly.
    ReadWrite,
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The viewer is not attached.
    UnknownViewer,
    /// The identity does not hold the lock.
    NotWriter,
    /// That identity is already attached.
    AlreadyAttached,
}

/// One item read from a viewer's bounded queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRead {
    /// Contiguous PTY output, beginning at `seq`.
    Output { seq: u64, bytes: Vec<u8> },
    /// Live output was lost; the viewer must request a fresh grid snapshot.
    Resync { oldest: u64 },
    /// Nothing is waiting.
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Viewer {
    mode: ViewerMode,
    size: Size,
    budget: NonZeroUsize,
    queued: usize,
    output: VecDeque<(u64, Vec<u8>)>,
    resync: Option<u64>,
    input_filter: ViewerInputFilter,
    input_since: Option<Duration>,
}

/// Attached viewers and the sole write-lock holder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewerRegistry {
    viewers: BTreeMap<ViewerId, Viewer>,
    writer: Option<Writer>,
}

impl ViewerRegistry {
    /// Number of attached viewers, including viewers waiting for a snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.viewers.len()
    }

    /// Whether no viewer is attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.viewers.is_empty()
    }

    /// Current lock holder.
    #[must_use]
    pub const fn writer(&self) -> Option<Writer> {
        self.writer
    }

    /// Attach one viewer without taking the lock.
    ///
    /// # Errors
    ///
    /// If that identity is already attached.
    pub fn attach(
        &mut self,
        id: ViewerId,
        mode: ViewerMode,
        size: Size,
        budget: NonZeroUsize,
    ) -> Result<(), Refusal> {
        if self.viewers.contains_key(&id) {
            return Err(Refusal::AlreadyAttached);
        }
        self.viewers.insert(
            id,
            Viewer {
                mode,
                size,
                budget,
                queued: 0,
                output: VecDeque::new(),
                resync: None,
                input_filter: ViewerInputFilter::default(),
                input_since: None,
            },
        );
        Ok(())
    }

    /// Detach a viewer, releasing the lock if it held it.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn detach(&mut self, id: ViewerId) -> Result<(), Refusal> {
        if self.viewers.remove(&id).is_none() {
            return Err(Refusal::UnknownViewer);
        }
        if self.writer == Some(Writer::Viewer(id)) {
            self.writer = None;
        }
        Ok(())
    }

    /// Explicitly transfer the lock to this identity and return its size when it is a viewer.
    /// A read-only viewer is promoted only by this operation.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn take(&mut self, who: Writer) -> Result<Option<Size>, Refusal> {
        let size = match who {
            Writer::Viewer(id) => {
                let viewer = self.viewers.get_mut(&id).ok_or(Refusal::UnknownViewer)?;
                viewer.mode = ViewerMode::ReadWrite;
                Some(viewer.size)
            }
            Writer::Program(_) => None,
        };
        self.writer = Some(who);
        Ok(size)
    }

    /// Release the lock, if held by `who`.
    ///
    /// # Errors
    ///
    /// If `who` does not hold the lock.
    pub fn release(&mut self, who: Writer) -> Result<(), Refusal> {
        if self.writer != Some(who) {
            return Err(Refusal::NotWriter);
        }
        self.writer = None;
        Ok(())
    }

    /// Check an input source against the lock.
    ///
    /// # Errors
    ///
    /// If the viewer is unknown or the source does not hold the lock.
    pub fn check_input(&self, who: Writer) -> Result<(), Refusal> {
        if let Writer::Viewer(id) = who
            && !self.viewers.contains_key(&id)
        {
            return Err(Refusal::UnknownViewer);
        }
        if self.writer != Some(who) {
            return Err(Refusal::NotWriter);
        }
        Ok(())
    }

    /// Strip terminal answers from a viewer's input, including split answers.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn filter_input(
        &mut self,
        id: ViewerId,
        bytes: &[u8],
        now: Duration,
    ) -> Result<Vec<u8>, Refusal> {
        let viewer = self.viewers.get_mut(&id).ok_or(Refusal::UnknownViewer)?;
        let filtered = viewer.input_filter.filter(bytes);
        if viewer.input_filter.has_pending() {
            viewer.input_since.get_or_insert(now);
        } else {
            viewer.input_since = None;
        }
        Ok(filtered)
    }

    /// The next deadline for forwarding an ambiguous standalone escape key.
    #[must_use]
    pub fn input_deadline(&self) -> Option<Duration> {
        self.viewers
            .values()
            .filter_map(|v| v.input_since.map(|at| at + Duration::from_millis(50)))
            .min()
    }

    /// Forward pending input whose 50 ms ambiguity window passed.
    pub fn flush_due_input(&mut self, now: Duration) -> Vec<(ViewerId, Vec<u8>)> {
        self.viewers
            .iter_mut()
            .filter_map(|(&id, viewer)| {
                let due = viewer
                    .input_since
                    .is_some_and(|at| at + Duration::from_millis(50) <= now);
                if !due {
                    return None;
                }
                viewer.input_since = None;
                Some((id, viewer.input_filter.flush()))
            })
            .collect()
    }

    /// Remember a viewer's size. Only the writer's request changes the PTY size.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn resize(&mut self, id: ViewerId, size: Size) -> Result<bool, Refusal> {
        let viewer = self.viewers.get_mut(&id).ok_or(Refusal::UnknownViewer)?;
        viewer.size = size;
        Ok(self.writer == Some(Writer::Viewer(id)))
    }

    /// Add output to each live viewer; return identities that first overflowed.
    pub fn output(&mut self, seq: u64, bytes: &[u8], oldest: u64) -> Vec<ViewerId> {
        let mut dropped = Vec::new();
        if bytes.is_empty() {
            return dropped;
        }
        for (&id, viewer) in &mut self.viewers {
            if viewer.resync.is_some() {
                continue;
            }
            if bytes.len() > viewer.budget.get().saturating_sub(viewer.queued) {
                viewer.output.clear();
                viewer.queued = 0;
                viewer.resync = Some(oldest);
                dropped.push(id);
            } else {
                viewer.queued += bytes.len();
                viewer.output.push_back((seq, bytes.to_vec()));
            }
        }
        dropped
    }

    /// Pop the next bounded output item.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn read(&mut self, id: ViewerId) -> Result<ViewerRead, Refusal> {
        let viewer = self.viewers.get_mut(&id).ok_or(Refusal::UnknownViewer)?;
        if let Some(oldest) = viewer.resync {
            return Ok(ViewerRead::Resync { oldest });
        }
        if let Some((seq, bytes)) = viewer.output.pop_front() {
            viewer.queued -= bytes.len();
            return Ok(ViewerRead::Output { seq, bytes });
        }
        Ok(ViewerRead::Empty)
    }

    /// Resume live output after a fresh grid snapshot has been made on the actor thread.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn resynced(&mut self, id: ViewerId) -> Result<(), Refusal> {
        let viewer = self.viewers.get_mut(&id).ok_or(Refusal::UnknownViewer)?;
        viewer.output.clear();
        viewer.queued = 0;
        viewer.resync = None;
        Ok(())
    }

    /// The queue's byte count, for checks and monitoring.
    #[must_use]
    pub fn queued(&self, id: ViewerId) -> Option<usize> {
        self.viewers.get(&id).map(|viewer| viewer.queued)
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, ViewerId, ViewerMode, ViewerRead, ViewerRegistry, Writer};
    use crate::emulator::Size;
    use proptest::prelude::*;
    use std::num::NonZeroUsize;

    fn size(cols: u16) -> Size {
        Size::new(cols, 24).unwrap()
    }

    fn attach(registry: &mut ViewerRegistry, id: u64, mode: ViewerMode, cols: u16, budget: usize) {
        registry
            .attach(
                ViewerId(id),
                mode,
                size(cols),
                NonZeroUsize::new(budget).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn taking_promotes_a_reader_and_transfers_size_ownership() {
        let mut registry = ViewerRegistry::default();
        attach(&mut registry, 1, ViewerMode::ReadWrite, 80, 10);
        attach(&mut registry, 2, ViewerMode::ReadOnly, 120, 10);
        assert_eq!(
            registry.check_input(Writer::Viewer(ViewerId(1))),
            Err(Refusal::NotWriter)
        );
        assert_eq!(
            registry.take(Writer::Viewer(ViewerId(1))),
            Ok(Some(size(80)))
        );
        assert_eq!(registry.resize(ViewerId(2), size(140)), Ok(false));
        assert_eq!(
            registry.take(Writer::Viewer(ViewerId(2))),
            Ok(Some(size(140)))
        );
        assert_eq!(
            registry.check_input(Writer::Viewer(ViewerId(1))),
            Err(Refusal::NotWriter)
        );
        assert_eq!(registry.check_input(Writer::Viewer(ViewerId(2))), Ok(()));
        assert_eq!(registry.resize(ViewerId(2), size(150)), Ok(true));
        registry.detach(ViewerId(2)).unwrap();
        assert_eq!(registry.writer(), None);
    }

    #[test]
    fn programmatic_input_requires_the_same_lock() {
        let mut registry = ViewerRegistry::default();
        attach(&mut registry, 1, ViewerMode::ReadWrite, 80, 10);
        registry.take(Writer::Viewer(ViewerId(1))).unwrap();
        assert_eq!(
            registry.check_input(Writer::Program(7)),
            Err(Refusal::NotWriter)
        );
        registry.take(Writer::Program(7)).unwrap();
        assert_eq!(
            registry.check_input(Writer::Viewer(ViewerId(1))),
            Err(Refusal::NotWriter)
        );
        assert_eq!(registry.check_input(Writer::Program(7)), Ok(()));
    }

    #[test]
    fn overflow_reports_resync_once_and_never_delivers_a_gap() {
        let mut registry = ViewerRegistry::default();
        attach(&mut registry, 1, ViewerMode::ReadOnly, 80, 4);
        assert_eq!(registry.output(0, b"abc", 0), Vec::<ViewerId>::new());
        assert_eq!(registry.output(3, b"de", 1), vec![ViewerId(1)]);
        assert!(registry.output(5, b"fg", 3).is_empty());
        assert_eq!(
            registry.read(ViewerId(1)),
            Ok(ViewerRead::Resync { oldest: 1 })
        );
        assert_eq!(registry.queued(ViewerId(1)), Some(0));
        registry.resynced(ViewerId(1)).unwrap();
        assert!(registry.output(7, b"hi", 5).is_empty());
        assert_eq!(
            registry.read(ViewerId(1)),
            Ok(ViewerRead::Output {
                seq: 7,
                bytes: b"hi".to_vec()
            })
        );
    }

    proptest! {
        #[test]
        fn queues_stay_within_budget_and_overflow_is_explicit(
            budget in 1usize..30,
            chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..20), 0..30),
        ) {
            let mut registry = ViewerRegistry::default();
            attach(&mut registry, 1, ViewerMode::ReadOnly, 80, budget);
            let mut seq = 0u64;
            for chunk in chunks {
                let dropped = registry.output(seq, &chunk, seq.saturating_sub(5));
                seq += chunk.len() as u64;
                prop_assert!(registry.queued(ViewerId(1)).unwrap() <= budget);
                if !dropped.is_empty() {
                    prop_assert_eq!(registry.read(ViewerId(1)).unwrap(), ViewerRead::Resync { oldest: (seq - chunk.len() as u64).saturating_sub(5) });
                    break;
                }
            }
        }

        #[test]
        fn only_one_identity_holds_the_lock(
            choices in proptest::collection::vec(0u8..4, 0..50),
        ) {
            let mut registry = ViewerRegistry::default();
            attach(&mut registry, 1, ViewerMode::ReadOnly, 80, 8);
            attach(&mut registry, 2, ViewerMode::ReadWrite, 100, 8);
            for choice in choices {
                let who = match choice { 0 => Writer::Viewer(ViewerId(1)), 1 => Writer::Viewer(ViewerId(2)), _ => Writer::Program(3) };
                if choice == 3 { let _ = registry.release(who); } else { registry.take(who).unwrap(); }
                let permitted = [Writer::Viewer(ViewerId(1)), Writer::Viewer(ViewerId(2)), Writer::Program(3)]
                    .into_iter().filter(|&candidate| registry.check_input(candidate).is_ok()).count();
                prop_assert!(permitted <= 1);
            }
        }
    }
}
