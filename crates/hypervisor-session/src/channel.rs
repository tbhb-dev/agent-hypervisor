//! Thin adapter between a decoded terminal channel and an already resolved session.

use std::num::NonZeroUsize;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use hypervisor_core::channel::{
    Control, ControlResult, Encoding, Event, Frame, FrameError, OpenRefusal, OpenRefused,
    OpenRequest, OpenResponse,
};
use hypervisor_core::channel_policy::{ChannelPolicy, ClientAction, ClientError};
use hypervisor_core::grid_channel::GridEncoder;
use hypervisor_core::session::{Refusal, ViewerId, Writer};

use crate::{SequencedEvent, SessionHandle};

const VIEWER_BUDGET: NonZeroUsize = NonZeroUsize::new(64 * 1024).unwrap();

/// A single open channel. The caller resolves the open target to `session` before opening.
pub struct Channel<'a> {
    session: &'a SessionHandle,
    viewer: ViewerId,
    events: Receiver<SequencedEvent>,
    policy: ChannelPolicy,
    grid: Option<GridEncoder>,
}

impl<'a> Channel<'a> {
    /// Open against a resolved existing or newly spawned session.
    ///
    /// # Errors
    /// An open refusal frame, including the supported version list, on failed negotiation.
    pub fn open(
        session: &'a SessionHandle,
        viewer: ViewerId,
        request: &OpenRequest,
    ) -> Result<(Self, Frame), Box<Frame>> {
        let policy = ChannelPolicy::open(request)
            .map_err(|refused| Box::new(Frame::OpenRefused(refused)))?;
        let grid = if request.encoding == Encoding::Grid {
            Some(
                GridEncoder::new(request.max_frames_per_second)
                    .map_err(|_| refusal(OpenRefusal::InvalidRequest))?,
            )
        } else {
            None
        };
        let size = request
            .size
            .into_session()
            .map_err(|_| refusal(OpenRefusal::InvalidRequest))?;
        let events = session.subscribe();
        let (effective_size, starting_sequence) = session
            .attach_channel(viewer, request.mode.into_viewer(), size, VIEWER_BUDGET)
            .map_err(|_| refusal(OpenRefusal::SessionRefused))?;
        let response = Frame::OpenResponse(OpenResponse::from_session(
            request.mode,
            effective_size,
            starting_sequence,
        ));
        Ok((
            Self {
                session,
                viewer,
                events,
                policy,
                grid,
            },
            response,
        ))
    }

    /// Poll the active grid using caller supplied monotonic time. The first result is full.
    ///
    /// # Errors
    /// If the channel is not grid encoded, the viewer is gone, or a frame cannot be encoded.
    pub fn poll_grid(&mut self, now: Duration) -> Result<Option<Frame>, ChannelError> {
        let grid = self
            .grid
            .as_mut()
            .ok_or(ChannelError::Client(ClientError::UnexpectedFrame))?;
        if self.policy.detached() {
            return Err(ChannelError::Client(ClientError::Detached));
        }
        if !grid.due(now).map_err(ChannelError::Grid)? {
            return Ok(None);
        }
        let Some((screen, modes, sequence, lost)) = self
            .session
            .viewer_grid(self.viewer)
            .map_err(ChannelError::Refused)?
        else {
            return Ok(None);
        };
        grid.poll(now, screen, modes, sequence, lost)
            .map(|frame| frame.map(Frame::Grid))
            .map_err(ChannelError::Grid)
    }

    /// Apply a client frame. Input is dropped when this channel is read-only or lacks the lock.
    ///
    /// # Errors
    /// A frame is out of place or the channel is detached.
    pub fn receive(&mut self, frame: Frame) -> Result<Option<Frame>, ChannelError> {
        let action = self.policy.client(frame).map_err(ChannelError::Client)?;
        let writer = Writer::Viewer(self.viewer);
        match action {
            ClientAction::DropInput => Ok(None),
            ClientAction::Input(bytes) => {
                let _ = self.session.submit(writer, bytes);
                Ok(None)
            }
            ClientAction::Resize(size) => {
                let size = size
                    .into_session()
                    .map_err(|_| ChannelError::Client(ClientError::InvalidSize))?;
                self.session
                    .viewer_resize(self.viewer, size)
                    .map_err(ChannelError::Refused)?;
                Ok(None)
            }
            ClientAction::Control(control) => {
                let result = match control {
                    Control::Take => self.session.take(writer),
                    Control::Release => self.session.release_writer(writer),
                    Control::Signal(signal) => self.session.signal(writer, signal.into_session()),
                    Control::Detach => self.session.detach(self.viewer),
                };
                self.policy.control_result(control, result.is_ok());
                Ok(Some(Frame::ControlResult(ControlResult::from_accepted(
                    result.is_ok(),
                ))))
            }
        }
    }

    /// Read one mapped server event without blocking. Events for other viewers are skipped.
    #[must_use]
    pub fn next_event(&self) -> Option<Frame> {
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    if let Some(mapped) = Event::from_session(event.event, self.viewer) {
                        return Some(Frame::Event(mapped));
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
            }
        }
    }
}

impl Drop for Channel<'_> {
    fn drop(&mut self) {
        if !self.policy.detached() {
            let _ = self.session.detach(self.viewer);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelError {
    Client(ClientError),
    Refused(Refusal),
    Grid(FrameError),
}

fn refusal(reason: OpenRefusal) -> Box<Frame> {
    Box::new(Frame::OpenRefused(OpenRefused {
        reason,
        supported_versions: vec![hypervisor_core::channel::VERSION],
    }))
}
