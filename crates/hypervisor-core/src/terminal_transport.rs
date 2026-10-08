//! Pure admission and delivery decisions for a local terminal listener.

use crate::channel::{Encoding, Event, MAX_FRAME, OpenTarget};

/// A peer can enter the listener only under its configured local UID.
#[must_use]
pub const fn admit_peer(peer_uid: u32, allowed_uid: u32) -> bool {
    peer_uid == allowed_uid
}

/// The listener path selects one already resolved session.
#[must_use]
pub fn targets_session(target: &OpenTarget, session_id: &str) -> bool {
    matches!(target, OpenTarget::Session(id) if id == session_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterEvent {
    Continue,
    ByteSnapshot,
    GridPoll,
}

/// Choose the recovery operation after the holder signals an overflow.
#[must_use]
pub const fn after_event(event: &Event, encoding: Encoding) -> AfterEvent {
    if matches!(event, Event::ResyncRequired { .. }) {
        match encoding {
            Encoding::Bytes => AfterEvent::ByteSnapshot,
            Encoding::Grid => AfterEvent::GridPoll,
        }
    } else {
        AfterEvent::Continue
    }
}

/// Bound encoded bytes waiting for one socket writer.
#[must_use]
pub const fn queue_admits(pending: usize, incoming: usize) -> bool {
    pending.saturating_add(incoming) <= 2 * (MAX_FRAME + 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn admission_target_recovery_and_queue_edges() {
        assert!(admit_peer(7, 7));
        assert!(!admit_peer(7, 8));
        assert!(targets_session(&OpenTarget::Session("s".into()), "s"));
        assert!(!targets_session(&OpenTarget::Session("other".into()), "s"));
        assert!(!targets_session(
            &OpenTarget::Spawn(crate::channel::WireSpawnSpec {
                command: "sh".into(),
                args: vec![],
                env: vec![],
                cwd: None,
                user: None,
                kind: crate::channel::SessionKind::Shell,
            }),
            "s"
        ));
        let resync = Event::ResyncRequired { oldest: 1 };
        assert_eq!(
            after_event(&resync, Encoding::Bytes),
            AfterEvent::ByteSnapshot
        );
        assert_eq!(after_event(&resync, Encoding::Grid), AfterEvent::GridPoll);
        assert!(queue_admits(2 * (MAX_FRAME + 4), 0));
        assert!(!queue_admits(2 * (MAX_FRAME + 4), 1));
    }

    proptest! {
        #[test]
        fn admission_is_uid_equality(peer in any::<u32>(), allowed in any::<u32>()) {
            prop_assert_eq!(admit_peer(peer, allowed), peer == allowed);
        }

        #[test]
        fn target_is_exact_session(id in ".{0,32}", selected in ".{0,32}") {
            prop_assert_eq!(targets_session(&OpenTarget::Session(id.clone()), &selected), id == selected);
        }

        #[test]
        fn queue_never_exceeds_bound(pending in any::<usize>(), incoming in any::<usize>()) {
            let admitted = queue_admits(pending, incoming);
            prop_assert_eq!(admitted, pending.checked_add(incoming).is_some_and(|sum| sum <= 2 * (MAX_FRAME + 4)));
        }

        #[test]
        fn resync_selects_encoding(oldest in any::<u64>(), grid in any::<bool>()) {
            let encoding = if grid { Encoding::Grid } else { Encoding::Bytes };
            let action = after_event(&Event::ResyncRequired { oldest }, encoding);
            prop_assert_eq!(action, if grid { AfterEvent::GridPoll } else { AfterEvent::ByteSnapshot });
        }
    }
}
