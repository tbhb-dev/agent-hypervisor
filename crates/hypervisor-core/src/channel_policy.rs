//! Pure admission and client-frame decisions for one terminal channel.

use crate::channel::{
    Control, Encoding, Frame, FrameError, Mode, OpenRefusal, OpenRefused, OpenRequest, VERSION,
    WireSize, negotiate,
};
use crate::session::RingRead;

/// The first byte frame after an accepted open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ByteStart {
    Replay(Vec<u8>),
    Snapshot,
    Ahead,
}

/// Why a grid-derived snapshot cannot be sent to a viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotRefusal {
    Empty,
    Frame(FrameError),
}

/// Validate the complete wire frame before the shell admits or resumes a viewer.
///
/// # Errors
/// An unavailable screen or a snapshot that cannot fit in one frame.
pub fn snapshot_frame(next_sequence: u64, bytes: Vec<u8>) -> Result<Frame, SnapshotRefusal> {
    if bytes.is_empty() {
        return Err(SnapshotRefusal::Empty);
    }
    let frame = Frame::Snapshot {
        next_sequence,
        bytes,
    };
    frame.encode().map_err(SnapshotRefusal::Frame)?;
    Ok(frame)
}

/// Choose replay only when every requested byte through the attachment point is retained.
#[must_use]
pub fn byte_start(requested: Option<u64>, attached_at: u64, read: Option<RingRead>) -> ByteStart {
    let Some(sequence) = requested else {
        return ByteStart::Snapshot;
    };
    if sequence > attached_at {
        return ByteStart::Ahead;
    }
    match read {
        Some(RingRead::Bytes(mut bytes)) => {
            if let Ok(required) = usize::try_from(attached_at - sequence)
                && bytes.len() >= required
            {
                bytes.truncate(required);
                ByteStart::Replay(bytes)
            } else {
                ByteStart::Snapshot
            }
        }
        Some(RingRead::Gone { .. }) | None => ByteStart::Snapshot,
        Some(RingRead::Ahead { .. }) => ByteStart::Ahead,
    }
}

/// Pure channel admission and client-frame decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelPolicy {
    mode: Mode,
    detached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientAction {
    DropInput,
    Input(Vec<u8>),
    Resize(WireSize),
    Control(Control),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientError {
    Detached,
    UnexpectedFrame,
    InvalidSize,
}

impl ChannelPolicy {
    /// Admit an open request after decoding, before attaching a viewer.
    ///
    /// # Errors
    /// Returns a version, encoding, or request refusal.
    pub fn open(request: &OpenRequest) -> Result<Self, OpenRefused> {
        negotiate(&request.versions)?;

        let reason = if request.encoding == Encoding::Grid && request.resume.is_some()
            || request.size.cols == 0
            || request.size.rows == 0
            || request.client.terminal.is_empty()
            || request.max_frames_per_second == Some(0)
            || request.encoding == Encoding::Bytes && request.max_frames_per_second.is_some()
        {
            Some(OpenRefusal::InvalidRequest)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(OpenRefused {
                reason,
                supported_versions: vec![VERSION],
            });
        }
        Ok(Self {
            mode: request.mode,
            detached: false,
        })
    }

    /// Decide what a client frame can do in the current mode.
    ///
    /// # Errors
    /// Returns an error for an out-of-place frame, invalid size, or detached channel.
    pub fn client(&self, frame: Frame) -> Result<ClientAction, ClientError> {
        if self.detached {
            return Err(ClientError::Detached);
        }
        match frame {
            Frame::Input(_) if self.mode == Mode::ReadOnly => Ok(ClientAction::DropInput),
            Frame::Input(bytes) => Ok(ClientAction::Input(bytes)),
            Frame::Resize(size) if size.cols == 0 || size.rows == 0 => {
                Err(ClientError::InvalidSize)
            }
            Frame::Resize(size) => Ok(ClientAction::Resize(size)),
            Frame::Control(control) => Ok(ClientAction::Control(control)),
            _ => Err(ClientError::UnexpectedFrame),
        }
    }

    /// Apply the holder's response to a control operation.
    pub fn control_result(&mut self, control: Control, accepted: bool) {
        if accepted {
            match control {
                Control::Take => self.mode = Mode::ReadWrite,
                Control::Detach => self.detached = true,
                Control::Release | Control::Signal(_) => {}
            }
        }
    }

    #[must_use]
    pub const fn detached(self) -> bool {
        self.detached
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Capabilities, OpenTarget, ResumeToken};
    use proptest::prelude::*;

    fn request() -> OpenRequest {
        OpenRequest {
            versions: vec![VERSION],
            target: OpenTarget::Session("s".into()),
            mode: Mode::ReadOnly,
            encoding: Encoding::Bytes,
            size: WireSize { cols: 80, rows: 24 },
            client: Capabilities {
                terminal: "test".into(),
                flags: 0,
            },
            resume: None,
            max_frames_per_second: None,
        }
    }

    #[test]
    fn channel_policy_drops_read_only_input_and_promotes_on_take() {
        let mut policy = ChannelPolicy::open(&request()).unwrap();
        assert_eq!(
            policy.client(Frame::Input(b"x".to_vec())),
            Ok(ClientAction::DropInput)
        );
        policy.control_result(Control::Take, false);
        assert_eq!(
            policy.client(Frame::Input(b"x".to_vec())),
            Ok(ClientAction::DropInput)
        );
        policy.control_result(Control::Take, true);
        assert_eq!(
            policy.client(Frame::Input(b"x".to_vec())),
            Ok(ClientAction::Input(b"x".to_vec()))
        );
        policy.control_result(Control::Detach, true);
        assert_eq!(
            policy.client(Frame::Control(Control::Release)),
            Err(ClientError::Detached)
        );
    }

    #[test]

    fn policy_accepts_grid_and_refuses_resume_and_bad_rate_or_size() {
        let mut open = request();
        open.encoding = Encoding::Grid;
        open.max_frames_per_second = Some(2);
        assert!(ChannelPolicy::open(&open).is_ok());
        open.max_frames_per_second = Some(0);
        assert_eq!(
            ChannelPolicy::open(&open).unwrap_err().reason,
            OpenRefusal::InvalidRequest
        );
        open.max_frames_per_second = None;
        open.encoding = Encoding::Bytes;
        open.resume = Some(ResumeToken { next_sequence: 0 });
        assert!(ChannelPolicy::open(&open).is_ok());
        open.resume = None;
        open.size.cols = 0;
        assert_eq!(
            ChannelPolicy::open(&open).unwrap_err().reason,
            OpenRefusal::InvalidRequest
        );
    }

    #[test]
    fn byte_start_handles_replay_snapshot_and_future() {
        assert_eq!(byte_start(None, 5, None), ByteStart::Snapshot);
        assert_eq!(
            byte_start(Some(2), 5, Some(RingRead::Bytes(b"cdef".to_vec()))),
            ByteStart::Replay(b"cde".to_vec())
        );
        assert_eq!(
            byte_start(Some(2), 5, Some(RingRead::Gone { oldest: 3 })),
            ByteStart::Snapshot
        );
        assert_eq!(
            byte_start(Some(2), 5, Some(RingRead::Bytes(b"cd".to_vec()))),
            ByteStart::Snapshot
        );
        assert_eq!(byte_start(Some(6), 5, None), ByteStart::Ahead);
    }

    #[test]
    fn snapshot_frame_refuses_empty_and_oversized_screens() {
        assert_eq!(snapshot_frame(3, vec![]), Err(SnapshotRefusal::Empty));
        assert_eq!(
            snapshot_frame(3, vec![b'x'; crate::channel::MAX_FRAME]),
            Err(SnapshotRefusal::Frame(FrameError::TooLarge))
        );
        assert_eq!(
            snapshot_frame(3, b"vt".to_vec()),
            Ok(Frame::Snapshot {
                next_sequence: 3,
                bytes: b"vt".to_vec()
            })
        );
    }

    proptest! {
        #[test]
        fn nonempty_snapshots_keep_the_sequence_and_bytes(
            sequence in any::<u64>(),
            bytes in proptest::collection::vec(any::<u8>(), 1..1000),
        ) {
            prop_assert_eq!(
                snapshot_frame(sequence, bytes.clone()),
                Ok(Frame::Snapshot { next_sequence: sequence, bytes })
            );
        }

        #[test]
        fn replay_ends_at_attachment_even_when_read_includes_later_output(
            sequence in 0u64..1_000_000,
            before in proptest::collection::vec(any::<u8>(), 0..1000),
            after in proptest::collection::vec(any::<u8>(), 0..1000),
        ) {
            let attached_at = sequence + before.len() as u64;
            let mut read = before.clone();
            read.extend_from_slice(&after);
            prop_assert_eq!(byte_start(Some(sequence), attached_at, Some(RingRead::Bytes(read))), ByteStart::Replay(before));
        }

        #[test]
        fn read_only_policy_never_forwards_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let policy = ChannelPolicy::open(&request()).unwrap();
            prop_assert_eq!(policy.client(Frame::Input(bytes)), Ok(ClientAction::DropInput));
        }
    }
}
