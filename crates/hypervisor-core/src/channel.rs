//! Version-one terminal channel values and transport-independent framing.

use crate::session::{SessionEvent, Signal, ViewerId, ViewerMode, Writer};
use crate::state::{AgentState, BlockReason};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::emulator::{PROFILE, Size};
/// The sole version implemented by this prototype.
pub const VERSION: u16 = 1;
/// Maximum frame length after the four-byte prefix, including the typed header.
pub const MAX_FRAME: usize = 1024 * 1024;
const HEADER: usize = 3;

/// A size in terminal cells, checked before it reaches the session holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSize {
    pub cols: u16,
    pub rows: u16,
}

impl WireSize {
    /// Convert a validated wire size into the holder's size.
    ///
    /// # Errors
    /// A zero dimension is invalid.
    pub const fn into_session(self) -> Result<Size, crate::emulator::ZeroSize> {
        Size::new(self.cols, self.rows)
    }
}

/// Client or server rendering capabilities. Unknown flag bits are ignored by version one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub terminal: String,
    pub flags: u32,
}

pub const CAP_ANSI_COLOR: u32 = 1;
pub const CAP_SGR_MOUSE: u32 = 1 << 1;
pub const CAP_BRACKETED_PASTE: u32 = 1 << 2;
pub const CAP_SYNC_OUTPUT: u32 = 1 << 3;

/// The fixed server profile as advertised to channel clients.
#[must_use]
pub fn server_capabilities() -> Capabilities {
    let modes = PROFILE.supported_modes;
    let mut flags = CAP_ANSI_COLOR;
    if modes.contains(&1006) {
        flags |= CAP_SGR_MOUSE;
    }
    if modes.contains(&2004) {
        flags |= CAP_BRACKETED_PASTE;
    }
    if modes.contains(&2026) {
        flags |= CAP_SYNC_OUTPUT;
    }
    Capabilities {
        terminal: PROFILE.version.into(),
        flags,
    }
}

/// A spawn target is carried by the wire, then resolved by the transport's session factory.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    pub user: Option<String>,
    pub kind: SessionKind,
}

impl fmt::Debug for WireSpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireSpawnSpec")
            .field("command", &self.command)
            .field("args", &self.args)
            .field(
                "env_names",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("cwd", &self.cwd)
            .field("user", &self.user)
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Agent,
    Shell,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OpenTarget {
    Session(String),
    Spawn(WireSpawnSpec),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

impl Mode {
    #[must_use]
    pub const fn into_viewer(self) -> ViewerMode {
        match self {
            Self::ReadOnly => ViewerMode::ReadOnly,
            Self::ReadWrite => ViewerMode::ReadWrite,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Bytes,
    Grid,
}

/// A byte offset to resume from. Replay and snapshots are run 14 work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeToken {
    pub next_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub versions: Vec<u16>,
    pub target: OpenTarget,
    pub mode: Mode,
    pub encoding: Encoding,
    pub size: WireSize,
    pub client: Capabilities,
    pub resume: Option<ResumeToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenResponse {
    pub version: u16,
    pub granted_mode: Mode,
    pub effective_size: WireSize,
    pub server: Capabilities,
    pub starting_sequence: u64,
}

impl OpenResponse {
    #[must_use]
    pub fn from_session(mode: Mode, size: Size, starting_sequence: u64) -> Self {
        Self {
            version: VERSION,
            granted_mode: mode,
            effective_size: WireSize {
                cols: size.cols(),
                rows: size.rows(),
            },
            server: server_capabilities(),
            starting_sequence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenRefusal {
    UnsupportedVersion,
    UnknownTarget,
    InvalidRequest,
    SessionRefused,
    UnsupportedEncoding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRefused {
    pub reason: OpenRefusal,
    pub supported_versions: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Take,
    Release,
    Signal(WireSignal),
    Detach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSignal {
    Hangup,
    Interrupt,
    Terminate,
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlResult {
    Accepted,
    Refused,
}

impl ControlResult {
    #[must_use]
    pub const fn from_accepted(accepted: bool) -> Self {
        if accepted {
            Self::Accepted
        } else {
            Self::Refused
        }
    }
}

/// Events are channel scoped, except session state and exit, which all viewers receive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Event {
    ModeChanged(Mode),
    SizeChanged(WireSize),
    WriterChanged(Option<WireWriter>),
    SessionStateChanged(String),
    SessionExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    ResyncRequired {
        oldest: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum WireWriter {
    Viewer(u64),
    Program(u64),
}

impl Event {
    /// Map a holder event to one channel's event, omitting unrelated viewer modes.
    #[must_use]
    pub fn from_session(event: SessionEvent, viewer: ViewerId) -> Option<Self> {
        match event {
            SessionEvent::ModeChanged { viewer: id, mode } if id == viewer => {
                Some(Self::ModeChanged(match mode {
                    ViewerMode::ReadOnly => Mode::ReadOnly,
                    ViewerMode::ReadWrite => Mode::ReadWrite,
                }))
            }
            SessionEvent::WriterChanged(writer) => {
                Some(Self::WriterChanged(writer.map(|who| match who {
                    Writer::Viewer(id) => WireWriter::Viewer(id.0),
                    Writer::Program(id) => WireWriter::Program(id),
                })))
            }
            SessionEvent::Resized(size) => Some(Self::SizeChanged(WireSize {
                cols: size.cols(),
                rows: size.rows(),
            })),
            SessionEvent::StateChanged { to, .. } => Some(Self::SessionStateChanged(
                match to {
                    AgentState::Unknown => "unknown",
                    AgentState::Idle => "idle",
                    AgentState::Working => "working",
                    AgentState::Blocked {
                        reason: BlockReason::Approval,
                    } => "blocked_approval",
                    AgentState::Blocked {
                        reason: BlockReason::Input,
                    } => "blocked_input",
                    AgentState::Blocked {
                        reason: BlockReason::Unknown,
                    } => "blocked_unknown",
                    AgentState::Exited => "exited",
                }
                .into(),
            )),
            SessionEvent::Exited(exit) => Some(Self::SessionExited {
                code: exit.code,
                signal: exit.signal,
            }),
            SessionEvent::ResyncRequired { viewer: id, oldest } if id == viewer => {
                Some(Self::ResyncRequired { oldest })
            }
            _ => None,
        }
    }
}

/// Version zero is reserved for the open request and refusal so peers can negotiate version one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    OpenRequest(OpenRequest),
    OpenResponse(OpenResponse),
    OpenRefused(OpenRefused),
    Input(Vec<u8>),
    Resize(WireSize),
    Control(Control),
    ControlResult(ControlResult),
    Event(Event),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    TooLarge,
    BadLength,
    UnknownType,
    UnsupportedVersion,
    MalformedPayload,
    InvalidValue,
}

impl WireSignal {
    #[must_use]
    pub const fn into_session(self) -> Signal {
        match self {
            Self::Hangup => Signal::Hangup,
            Self::Interrupt => Signal::Interrupt,
            Self::Terminate => Signal::Terminate,
            Self::Kill => Signal::Kill,
        }
    }
}

impl Frame {
    /// Encode a complete frame. The prefix counts the typed header and payload, not itself.
    ///
    /// # Errors
    /// When a value is invalid or exceeds the size limit.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        self.validate()?;
        let (version, kind, payload) = match self {
            Self::OpenRequest(v) => (0, 1, json(v)?),
            Self::OpenResponse(v) => (VERSION, 2, json(v)?),
            Self::OpenRefused(v) => (0, 3, json(v)?),
            Self::Input(v) => (VERSION, 4, v.clone()),
            Self::Resize(v) => (VERSION, 5, json(v)?),
            Self::Control(v) => (VERSION, 6, json(v)?),
            Self::ControlResult(v) => (VERSION, 7, json(v)?),
            Self::Event(v) => (VERSION, 8, json(v)?),
        };
        let length = HEADER + payload.len();
        if length > MAX_FRAME {
            return Err(FrameError::TooLarge);
        }
        let mut bytes = Vec::with_capacity(4 + length);
        bytes.extend_from_slice(
            &u32::try_from(length)
                .map_err(|_| FrameError::TooLarge)?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.push(kind);
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    /// Decode one frame from a stream buffer, returning `None` for an incomplete frame.
    ///
    /// # Errors
    /// When the header or the complete payload is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Option<(Self, usize)>, FrameError> {
        let Some(prefix) = bytes.get(..4) else {
            return Ok(None);
        };
        let length = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
        if length < HEADER {
            return Err(FrameError::BadLength);
        }
        if length > MAX_FRAME {
            return Err(FrameError::TooLarge);
        }
        let Some(body) = bytes.get(4..4 + length) else {
            return Ok(None);
        };
        let version = u16::from_be_bytes([body[0], body[1]]);
        let kind = body[2];
        if !matches!(kind, 1 | 3) && version != VERSION || matches!(kind, 1 | 3) && version != 0 {
            return Err(FrameError::UnsupportedVersion);
        }
        let payload = &body[HEADER..];
        let frame = match kind {
            1 => Self::OpenRequest(parse(payload)?),
            2 => Self::OpenResponse(parse(payload)?),
            3 => Self::OpenRefused(parse(payload)?),
            4 => Self::Input(payload.to_vec()),
            5 => Self::Resize(parse(payload)?),
            6 => Self::Control(parse(payload)?),
            7 => Self::ControlResult(parse(payload)?),
            8 => Self::Event(parse(payload)?),
            _ => return Err(FrameError::UnknownType),
        };
        frame.validate()?;
        Ok(Some((frame, length + 4)))
    }

    fn validate(&self) -> Result<(), FrameError> {
        match self {
            Self::OpenRequest(v) => {
                if v.versions.is_empty()
                    || v.size.cols == 0
                    || v.size.rows == 0
                    || v.client.terminal.is_empty()
                {
                    return Err(FrameError::InvalidValue);
                }
                match &v.target {
                    OpenTarget::Session(id) if id.is_empty() => {
                        return Err(FrameError::InvalidValue);
                    }
                    OpenTarget::Spawn(spec) if spec.command.is_empty() => {
                        return Err(FrameError::InvalidValue);
                    }
                    _ => {}
                }
            }
            Self::OpenResponse(v)
                if v.version != VERSION
                    || v.effective_size.cols == 0
                    || v.effective_size.rows == 0 =>
            {
                return Err(FrameError::InvalidValue);
            }
            Self::Resize(v) if v.cols == 0 || v.rows == 0 => return Err(FrameError::InvalidValue),
            _ => {}
        }
        Ok(())
    }
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    serde_json::to_vec(value).map_err(|_| FrameError::MalformedPayload)
}

fn parse<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, FrameError> {
    serde_json::from_slice(bytes).map_err(|_| FrameError::MalformedPayload)
}

/// Select the highest mutually supported version, currently version one only.
/// # Errors
/// Returns the supported version list when no offered version is implemented.
pub fn negotiate(versions: &[u16]) -> Result<u16, OpenRefused> {
    if versions.contains(&VERSION) {
        Ok(VERSION)
    } else {
        Err(OpenRefused {
            reason: OpenRefusal::UnsupportedVersion,
            supported_versions: vec![VERSION],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn request(target: OpenTarget) -> OpenRequest {
        OpenRequest {
            versions: vec![2, VERSION],
            target,
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
    fn all_frame_types_round_trip_with_trailing_bytes() {
        let frames = [
            Frame::OpenRequest(request(OpenTarget::Session("s1".into()))),
            Frame::OpenRequest(request(OpenTarget::Spawn(WireSpawnSpec {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "true".into()],
                env: vec![("PATH".into(), "/bin".into())],
                cwd: Some("/".into()),
                user: None,
                kind: SessionKind::Shell,
            }))),
            Frame::OpenResponse(OpenResponse {
                version: VERSION,
                granted_mode: Mode::ReadOnly,
                effective_size: WireSize { cols: 80, rows: 24 },
                server: Capabilities {
                    terminal: "agent-hypervisor".into(),
                    flags: 0,
                },
                starting_sequence: 45,
            }),
            Frame::OpenRefused(OpenRefused {
                reason: OpenRefusal::UnsupportedVersion,
                supported_versions: vec![VERSION],
            }),
            Frame::Input(vec![0, 255, 3]),
            Frame::Resize(WireSize {
                cols: 120,
                rows: 30,
            }),
            Frame::Control(Control::Take),
            Frame::Control(Control::Release),
            Frame::Control(Control::Signal(WireSignal::Interrupt)),
            Frame::Control(Control::Signal(WireSignal::Hangup)),
            Frame::Control(Control::Signal(WireSignal::Terminate)),
            Frame::Control(Control::Signal(WireSignal::Kill)),
            Frame::Control(Control::Detach),
            Frame::ControlResult(ControlResult::Refused),
            Frame::Event(Event::ModeChanged(Mode::ReadWrite)),
            Frame::Event(Event::SizeChanged(WireSize { cols: 90, rows: 24 })),
            Frame::Event(Event::WriterChanged(Some(WireWriter::Viewer(4)))),
            Frame::Event(Event::SessionStateChanged("working".into())),
            Frame::Event(Event::SessionExited {
                code: Some(7),
                signal: None,
            }),
            Frame::Event(Event::ResyncRequired { oldest: 10 }),
        ];
        for frame in frames {
            let encoded = frame.encode().unwrap();
            let mut with_tail = encoded.clone();
            with_tail.extend_from_slice(b"tail");
            assert_eq!(Frame::decode(&with_tail), Ok(Some((frame, encoded.len()))));
            for end in 0..encoded.len() {
                assert_eq!(Frame::decode(&encoded[..end]), Ok(None));
            }
        }
    }

    #[test]
    fn version_negotiation_refuses_unsupported_versions() {
        assert_eq!(negotiate(&[2, 1]), Ok(1));
        assert_eq!(
            negotiate(&[2]),
            Err(OpenRefused {
                reason: OpenRefusal::UnsupportedVersion,
                supported_versions: vec![1]
            })
        );
        let mut bytes = Frame::Input(vec![1]).encode().unwrap();
        bytes[4..6].copy_from_slice(&2u16.to_be_bytes());
        assert_eq!(Frame::decode(&bytes), Err(FrameError::UnsupportedVersion));
    }

    #[test]
    fn malformed_length_type_payload_and_values_are_refused() {
        assert_eq!(
            Frame::decode(&2u32.to_be_bytes()),
            Err(FrameError::BadLength)
        );
        assert_eq!(
            Frame::decode(&(u32::try_from(MAX_FRAME).unwrap() + 1).to_be_bytes()),
            Err(FrameError::TooLarge)
        );
        let mut bytes = Frame::Control(Control::Take).encode().unwrap();
        bytes[6] = 99;
        assert_eq!(Frame::decode(&bytes), Err(FrameError::UnknownType));
        bytes[6] = 6;
        bytes[7] = b'!';
        assert_eq!(Frame::decode(&bytes), Err(FrameError::MalformedPayload));
        assert_eq!(
            Frame::Resize(WireSize { cols: 0, rows: 24 }).encode(),
            Err(FrameError::InvalidValue)
        );
        let mut bytes = Frame::Resize(WireSize { cols: 80, rows: 24 })
            .encode()
            .unwrap();
        let index = bytes.windows(2).position(|v| v == b"80").unwrap();
        bytes[index..index + 2].copy_from_slice(b" 0");
        assert_eq!(Frame::decode(&bytes), Err(FrameError::InvalidValue));
    }

    #[test]
    fn spawn_debug_hides_environment_values() {
        let spec = WireSpawnSpec {
            command: "/bin/sh".into(),
            args: Vec::new(),
            env: vec![("TOKEN".into(), "sensitive-value".into())],
            cwd: None,
            user: None,
            kind: SessionKind::Shell,
        };
        let debug = format!("{spec:?}");
        assert!(debug.contains("TOKEN"));
        assert!(!debug.contains("sensitive-value"));
    }

    #[test]
    fn advertised_server_capabilities_follow_the_fixed_profile() {
        let profile = server_capabilities();
        assert_eq!(profile.terminal, PROFILE.version);
        assert_eq!(
            profile.flags,
            CAP_ANSI_COLOR | CAP_SGR_MOUSE | CAP_BRACKETED_PASTE | CAP_SYNC_OUTPUT
        );
    }

    proptest! {
        #[test]
        fn arbitrary_input_bytes_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let frame = Frame::Input(bytes);
            let encoded = frame.encode().unwrap();
            prop_assert_eq!(Frame::decode(&encoded), Ok(Some((frame, encoded.len()))));
        }

        #[test]
        fn arbitrary_malformed_stream_data_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = Frame::decode(&bytes);
        }


    }
}
