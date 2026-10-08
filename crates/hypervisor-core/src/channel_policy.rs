//! Pure admission and client-frame decisions for one terminal channel.

use crate::channel::{
    Control, Encoding, Frame, Mode, OpenRefusal, OpenRefused, OpenRequest, VERSION, WireSize,
    negotiate,
};

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
        let reason = if request.encoding != Encoding::Bytes {
            Some(OpenRefusal::UnsupportedEncoding)
        } else if request.resume.is_some()
            || request.size.cols == 0
            || request.size.rows == 0
            || request.client.terminal.is_empty()
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
    fn policy_refuses_grid_resume_and_bad_size() {
        let mut open = request();
        open.encoding = Encoding::Grid;
        assert_eq!(
            ChannelPolicy::open(&open).unwrap_err().reason,
            OpenRefusal::UnsupportedEncoding
        );
        open.encoding = Encoding::Bytes;
        open.resume = Some(ResumeToken { next_sequence: 0 });
        assert_eq!(
            ChannelPolicy::open(&open).unwrap_err().reason,
            OpenRefusal::InvalidRequest
        );
        open.resume = None;
        open.size.cols = 0;
        assert_eq!(
            ChannelPolicy::open(&open).unwrap_err().reason,
            OpenRefusal::InvalidRequest
        );
    }

    proptest! {
        #[test]
        fn read_only_policy_never_forwards_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let policy = ChannelPolicy::open(&request()).unwrap();
            prop_assert_eq!(policy.client(Frame::Input(bytes)), Ok(ClientAction::DropInput));
        }
    }
}
