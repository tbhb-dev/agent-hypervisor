//! The session actor: one thread per session that owns its PTY and its emulator.
//!
//! [`spawn`] starts a thread that creates the emulator on itself, so an emulator that is neither
//! `Send` nor `Sync`, like `GhosttyEmulator`, never leaves it. The thread spawns the PTY, then
//! runs [`Holder::step`] over every message and carries out the effects it returns. Commands and
//! events cross the thread boundary as plain values over channels. A reader thread and a waiter
//! thread feed PTY output and the child's exit into the same channel, so output keeps flowing
//! into the ring and the emulator, and queries keep getting answers, with no viewer attached.
//!
//! This crate decides nothing. The holder decides; the actor reads the clock, moves bytes, and
//! calls the backend. Session events and failed effects are logged with workload and session ids.

use std::fmt;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hypervisor_core::emulator::{Cursor, Emulator, Grid, Modes, PROFILE, QueryScanner, Size};
use hypervisor_core::screen::{RuleDetector, ScreenDetector};
use hypervisor_core::session::{
    Effect, Exit, Holder, HolderConfig, Input, Phase, Refusal, RingRead, SessionEvent, Signal,
    SpawnSpec, ViewerId, ViewerMode, ViewerRead, Writer,
};
use hypervisor_core::state::{Harness, HookKind, HookReport};
use hypervisor_pty::{Pty, PtyError, Spawn, Wait};
use serde_json::json;
use std::num::NonZeroUsize;

pub mod channel;

/// The most messages handled before pending size requests settle.
const BATCH: usize = 64;
type GridSnapshot = (Grid, Modes, u64, bool);

/// A request to a running session.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Add a subscriber for subsequent ordered events.
    Subscribe(Sender<SequencedEvent>),
    /// Enable screen detection for a known harness, or disable it with `None`.
    DetectScreen(Option<Harness>),
    /// A normalized event from this session's hook listener.
    Hook {
        harness: Harness,
        kind: HookKind,
        seq: u64,
    },
    /// Attach a viewer without taking the lock.
    Attach(
        ViewerId,
        ViewerMode,
        Size,
        NonZeroUsize,
        Sender<Result<(), Refusal>>,
    ),
    /// Attach and report the size and output sequence in the same actor turn.
    AttachChannel(
        ViewerId,
        ViewerMode,
        Size,
        NonZeroUsize,
        Sender<Result<(Size, u64), Refusal>>,
    ),
    /// Detach a viewer.
    Detach(ViewerId, Sender<Result<(), Refusal>>),
    /// Explicitly take the lock.
    Take(Writer, Sender<Result<(), Refusal>>),
    /// Release the lock.
    ReleaseWriter(Writer, Sender<Result<(), Refusal>>),
    /// Request a size; only the writer changes the PTY.
    ViewerResize(ViewerId, Size, Sender<Result<(), Refusal>>),
    /// Send viewer or programmatic input under the same lock.
    Submit(Writer, Vec<u8>, Sender<Result<(), Refusal>>),
    /// Pop a viewer's next bounded output item.
    ReadViewer(ViewerId, Sender<Result<ViewerRead, Refusal>>),
    /// Return a grid snapshot and the next output sequence, then resume live output.
    ViewerSnapshot(ViewerId, Sender<Result<(u64, Vec<u8>), Refusal>>),
    /// Capture the active grid, modes, and byte offset in one actor turn.
    ViewerGrid(ViewerId, Sender<Result<Option<GridSnapshot>, Refusal>>),
    /// Interrupt from the lock holder.
    Interrupt(Writer, Sender<Result<(), Refusal>>),
    /// Signal the foreground group from the write-lock holder.
    Signal(Writer, Signal, Sender<Result<(), Refusal>>),
    /// End the session.
    Close,
    /// Read the output ring from a sequence number.
    ReadFrom(u64, Sender<RingRead>),
    /// The screen as VT bytes, or `None` once the session is reaped.
    Snapshot(Sender<Option<Vec<u8>>>),
}

#[cfg(test)]
mod log_tests {
    use super::*;
    use hypervisor_core::channel::{
        Capabilities, Encoding, Mode, OpenRequest, OpenTarget, WireSize,
    };
    use hypervisor_core::channel_policy::ClientError;
    use hypervisor_core::session::{Persistence, Signal, Target, ViewerRead};
    use hypervisor_ghostty::GhosttyEmulator;
    use hypervisor_pty::{Caps, Resize, Support};
    use serde_json::Value;
    use std::io;

    struct FailingWrite;

    struct NoWait;

    impl Wait for NoWait {
        fn wait(self) -> io::Result<Exit> {
            unreachable!()
        }
    }

    impl Pty for FailingWrite {
        type Reader = io::Empty;
        type Waiter = NoWait;

        fn reader(&self) -> io::Result<Self::Reader> {
            Ok(io::empty())
        }

        fn take_waiter(&mut self) -> Option<Self::Waiter> {
            None
        }

        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected write failure",
            ))
        }

        fn resize(&mut self, _: Size) -> io::Result<Resize> {
            Ok(Resize::Unchanged)
        }

        fn signal(&self, _: Signal, _: Target) -> io::Result<Support> {
            Ok(Support::Ok)
        }

        fn redraw_hint(&self) -> io::Result<Support> {
            Ok(Support::Ok)
        }

        fn foreground(&self) -> Option<i32> {
            None
        }

        fn caps(&self) -> Caps {
            Caps {
                foreground: false,
                signals: true,
                redraw_hint: true,
                backend_queries: &[],
                resize_repaints: false,
            }
        }
    }

    #[test]
    fn grid_open_skips_byte_snapshot_and_byte_methods_are_refused() {
        let (tx, rx) = mpsc::channel();
        let (_events_tx, events) = mpsc::channel();
        let thread = thread::spawn(move || {
            while let Ok(Msg::Command(command)) = rx.recv() {
                match command {
                    Command::Subscribe(_) => {}
                    Command::AttachChannel(_, _, size, _, reply) => {
                        let _ = reply.send(Ok((size, 0)));
                    }
                    Command::Detach(_, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    Command::Close => break,
                    other => panic!("unexpected byte command on grid channel: {other:?}"),
                }
            }
        });
        let session = SessionHandle {
            tx,
            events,
            thread: Some(thread),
        };
        let request = OpenRequest {
            versions: vec![1],
            target: OpenTarget::Session("test".into()),
            mode: Mode::ReadOnly,
            encoding: Encoding::Grid,
            size: WireSize { cols: 1, rows: 1 },
            client: Capabilities {
                terminal: "test".into(),
                flags: 0,
            },
            resume: None,
            max_frames_per_second: None,
        };
        let (mut channel, _) = channel::Channel::open(&session, ViewerId(42), &request).unwrap();
        assert_eq!(
            channel.next_output(),
            Err(channel::ChannelError::Client(ClientError::UnexpectedFrame))
        );
        assert_eq!(
            channel.snapshot(),
            Err(channel::ChannelError::Client(ClientError::UnexpectedFrame))
        );
        drop(channel);
        session.send(Command::Close);
        session.join();
    }

    #[test]
    fn channel_attach_reports_the_first_queued_output_sequence() {
        let mut actor: Actor<FailingWrite, GhosttyEmulator> = Actor {
            holder: Holder::new(
                HolderConfig::new(Persistence::Persistent),
                Size::new(80, 24).unwrap(),
            ),
            pty: Some(FailingWrite),
            emulator: None,
            events: Vec::new(),
            event_seq: 0,
            origin: Instant::now(),
            log: LogContext {
                workload_id: "test".into(),
                session_id: "test".into(),
            },
            queries: QueryScanner::default(),
            screen_harness: None,
            screen_at: None,
            error_lines: Vec::new(),
        };
        actor.step(Input::Spawned).unwrap();
        actor.handle(Msg::Output(b"before".to_vec()));
        let viewer = ViewerId(1);
        let (reply, answer) = mpsc::channel();
        actor.handle(Msg::Command(Command::AttachChannel(
            viewer,
            ViewerMode::ReadOnly,
            Size::new(80, 24).unwrap(),
            NonZeroUsize::new(64).unwrap(),
            reply,
        )));
        actor.handle(Msg::Output(b"after".to_vec()));
        let (size, starting_sequence) = answer.recv().unwrap().unwrap();
        assert_eq!(size, Size::new(80, 24).unwrap());
        assert_eq!(starting_sequence, 6);
        assert_eq!(
            actor.holder.read_viewer(viewer),
            Ok(ViewerRead::Output {
                seq: starting_sequence,
                bytes: b"after".to_vec(),
            })
        );
    }

    #[test]
    fn failed_pty_write_is_logged_with_session_ids() {
        let mut actor: Actor<FailingWrite, GhosttyEmulator> = Actor {
            holder: Holder::new(
                HolderConfig::new(Persistence::Persistent),
                Size::new(80, 24).unwrap(),
            ),
            pty: Some(FailingWrite),
            emulator: None,
            events: Vec::new(),
            event_seq: 0,
            origin: Instant::now(),
            log: LogContext {
                workload_id: "workload-1".into(),
                session_id: "session-2".into(),
            },
            queries: QueryScanner::default(),
            screen_harness: None,
            screen_at: None,
            error_lines: Vec::new(),
        };
        actor.apply(Effect::WritePty(b"input".to_vec()));
        assert_eq!(actor.error_lines.len(), 1);
        let value: Value = serde_json::from_str(&actor.error_lines[0]).unwrap();
        assert_eq!(value["workload_id"], "workload-1");
        assert_eq!(value["session_id"], "session-2");
        assert_eq!(value["operation"], "write_pty");
        assert_eq!(value["error"], "injected write failure");
    }

    #[test]
    fn event_log_has_ids_and_structured_channel() {
        let context = LogContext {
            workload_id: "workload-1".into(),
            session_id: "session-2".into(),
        };
        let attached = serde_json::from_str::<Value>(&context.event_line(SequencedEvent {
            seq: 3,
            event: SessionEvent::Attached {
                viewer: ViewerId(4),
            },
        }))
        .unwrap();
        assert_eq!(attached["workload_id"], "workload-1");
        assert_eq!(attached["session_id"], "session-2");
        assert_eq!(attached["channel_id"], "viewer:4");
        assert_eq!(attached["event_seq"], 3);
        assert_eq!(attached["event_type"], "attached");
        assert_eq!(attached["event"], "Attached { viewer: ViewerId(4) }");
        let created = serde_json::from_str::<Value>(&context.event_line(SequencedEvent {
            seq: 1,
            event: SessionEvent::Created,
        }))
        .unwrap();
        assert!(created["channel_id"].is_null());
    }

    #[test]
    fn log_json_escapes_caller_supplied_ids() {
        let context = LogContext {
            workload_id: "workload\"\nname".into(),
            session_id: "session\\name".into(),
        };
        let line = context.event_line(SequencedEvent {
            seq: 1,
            event: SessionEvent::Created,
        });
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["workload_id"], context.workload_id);
        assert_eq!(value["session_id"], context.session_id);
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn signal_error_line_carries_session_ids() {
        let context = LogContext {
            workload_id: "workload-1".into(),
            session_id: "session-2".into(),
        };
        let line = context.error_line("signal", &"permission denied");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["workload_id"], "workload-1");
        assert_eq!(value["session_id"], "session-2");
        assert!(value["channel_id"].is_null());
        assert_eq!(value["operation"], "signal");
        assert_eq!(value["error"], "permission denied");
    }
}

enum Msg {
    Command(Command),
    Output(Vec<u8>),
    OutputClosed,
    Exited(Exit),
}

/// Why a session did not start.
#[derive(Debug)]
pub enum StartError {
    /// The PTY backend refused the spec or failed.
    Pty(PtyError),
    /// The emulator could not be created.
    Emulator(String),
    /// The session thread could not be started or died before reporting.
    Thread(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pty(e) => e.fmt(f),
            Self::Emulator(e) => write!(f, "emulator: {e}"),
            Self::Thread(e) => write!(f, "session thread: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// One session event with a sequence assigned by its actor before fanout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: SessionEvent,
}

/// Identities supplied by the caller for structured session logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogContext {
    pub workload_id: String,
    pub session_id: String,
}

impl LogContext {
    fn error_line(&self, operation: &str, error: &impl fmt::Display) -> String {
        json!({
            "level": "error",
            "workload_id": self.workload_id,
            "session_id": self.session_id,
            "channel_id": null,
            "event_type": "effect_error",
            "operation": operation,
            "error": error.to_string(),
        })
        .to_string()
    }

    /// Serialize one event as a JSON log line. A viewer is the local channel until the
    /// terminal channel protocol assigns independent channel identities in Phase 2.
    #[must_use]
    fn event_line(&self, envelope: SequencedEvent) -> String {
        let (event_type, channel_id) = match envelope.event {
            SessionEvent::Created => ("created", None),
            SessionEvent::Attached { viewer } => ("attached", Some(format!("viewer:{}", viewer.0))),
            SessionEvent::Detached { viewer } => ("detached", Some(format!("viewer:{}", viewer.0))),
            SessionEvent::StateChanged { .. } => ("state_changed", None),
            SessionEvent::Running => ("running", None),
            SessionEvent::Resized(_) => ("resized", None),
            SessionEvent::ModeChanged { viewer, .. } => {
                ("mode_changed", Some(format!("viewer:{}", viewer.0)))
            }
            SessionEvent::WriterChanged(_) => ("writer_changed", None),
            SessionEvent::ResyncRequired { viewer, .. } => {
                ("resync_required", Some(format!("viewer:{}", viewer.0)))
            }
            SessionEvent::Exited(_) => ("exited", None),
            SessionEvent::Reaped => ("reaped", None),
            _ => ("unknown", None),
        };
        json!({
            "level": "info",
            "workload_id": self.workload_id,
            "session_id": self.session_id,
            "channel_id": channel_id,
            "event_seq": envelope.seq,
            "event_type": event_type,
            "event": format!("{:?}", envelope.event),
        })
        .to_string()
    }
}

/// The caller's side of a session. Dropping it closes the session.
pub struct SessionHandle {
    tx: Sender<Msg>,
    events: Receiver<SequencedEvent>,
    thread: Option<JoinHandle<()>>,
}

impl SessionHandle {
    fn request(
        &self,
        make: impl FnOnce(Sender<Result<(), Refusal>>) -> Command,
    ) -> Result<(), Refusal> {
        let (reply, answer) = mpsc::channel();
        self.send(make(reply));
        answer.recv().unwrap_or(Err(Refusal::UnknownViewer))
    }

    /// Attach a viewer with a bounded output queue.
    ///
    /// # Errors
    ///
    /// If the viewer is already attached or the actor has ended.
    pub fn attach(
        &self,
        id: ViewerId,
        mode: ViewerMode,
        size: Size,
        budget: NonZeroUsize,
    ) -> Result<(), Refusal> {
        self.request(|reply| Command::Attach(id, mode, size, budget, reply))
    }

    /// Attach a channel and return the size and sequence at that attachment point.
    ///
    /// # Errors
    /// If the viewer is already attached or the actor has ended.
    pub fn attach_channel(
        &self,
        id: ViewerId,
        mode: ViewerMode,
        size: Size,
        budget: NonZeroUsize,
    ) -> Result<(Size, u64), Refusal> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::AttachChannel(id, mode, size, budget, reply));
        answer.recv().unwrap_or(Err(Refusal::UnknownViewer))
    }

    /// Detach a viewer.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached or the actor has ended.
    pub fn detach(&self, id: ViewerId) -> Result<(), Refusal> {
        self.request(|reply| Command::Detach(id, reply))
    }

    /// Explicitly transfer the write lock.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached or the actor has ended.
    pub fn take(&self, who: Writer) -> Result<(), Refusal> {
        self.request(|reply| Command::Take(who, reply))
    }

    /// Release the write lock.
    ///
    /// # Errors
    ///
    /// If `who` does not hold it or the actor has ended.
    pub fn release_writer(&self, who: Writer) -> Result<(), Refusal> {
        self.request(|reply| Command::ReleaseWriter(who, reply))
    }

    /// Remember a viewer size and apply it if that viewer holds the lock.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached or the actor has ended.
    pub fn viewer_resize(&self, id: ViewerId, size: Size) -> Result<(), Refusal> {
        self.request(|reply| Command::ViewerResize(id, size, reply))
    }

    /// Send viewer or programmatic input under the same lock.
    ///
    /// # Errors
    ///
    /// If `who` does not hold the lock or the actor has ended.
    pub fn submit(&self, who: Writer, bytes: Vec<u8>) -> Result<(), Refusal> {
        self.request(|reply| Command::Submit(who, bytes, reply))
    }

    /// Interrupt the foreground job under the same lock.
    ///
    /// # Errors
    ///
    /// If `who` does not hold the lock or the actor has ended.
    pub fn interrupt(&self, who: Writer) -> Result<(), Refusal> {
        self.request(|reply| Command::Interrupt(who, reply))
    }

    /// Signal the foreground group under the same write lock.
    ///
    /// # Errors
    /// If the caller does not hold the lock or the actor has ended.
    pub fn signal(&self, who: Writer, signal: Signal) -> Result<(), Refusal> {
        self.request(|reply| Command::Signal(who, signal, reply))
    }

    /// Pop the next output item, including a resync notice after overflow.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached or the actor has ended.
    pub fn read_viewer(&self, id: ViewerId) -> Result<ViewerRead, Refusal> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::ReadViewer(id, reply));
        answer.recv().unwrap_or(Err(Refusal::UnknownViewer))
    }

    /// Take a fresh grid snapshot at an output sequence and resume live delivery.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached or the actor has ended.
    pub fn viewer_snapshot(&self, id: ViewerId) -> Result<(u64, Vec<u8>), Refusal> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::ViewerSnapshot(id, reply));
        answer.recv().unwrap_or(Err(Refusal::UnknownViewer))
    }
    /// Return a grid snapshot and clear this grid viewer's byte queue.
    ///
    /// # Errors
    /// If the viewer is not attached or the actor has ended.
    pub fn viewer_grid(&self, id: ViewerId) -> Result<Option<GridSnapshot>, Refusal> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::ViewerGrid(id, reply));
        answer.recv().unwrap_or(Err(Refusal::UnknownViewer))
    }
    /// Sends a command. A command sent after the session thread has ended is dropped; the
    /// event channel's disconnection reports that end.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Command(command));
    }

    /// The session's events, in order. The channel disconnects when the session is reaped.
    #[must_use]
    pub const fn events(&self) -> &Receiver<SequencedEvent> {
        &self.events
    }

    /// Subscribe to events emitted after this command reaches the actor.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<SequencedEvent> {
        let (tx, rx) = mpsc::channel();
        self.send(Command::Subscribe(tx));
        rx
    }

    /// Reads the output ring from `seq`, or `None` once the session thread has ended.
    #[must_use]
    pub fn read_from(&self, seq: u64) -> Option<RingRead> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::ReadFrom(seq, reply));
        answer.recv().ok()
    }

    /// The screen as VT bytes, or `None` once the session is reaped.
    #[must_use]
    pub fn snapshot(&self) -> Option<Vec<u8>> {
        let (reply, answer) = mpsc::channel();
        self.send(Command::Snapshot(reply));
        answer.recv().ok().flatten()
    }

    /// Waits for the session thread to end, which it does once the session is reaped.
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            // A panic on the session thread already ended the session; there is nothing to add.
            let _ = thread.join();
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.send(Command::Close);
    }
}

/// Starts a session thread, which creates the emulator with `make_emulator`, spawns `spec` with
/// `spawner`, and runs until the session is reaped.
///
/// # Errors
///
/// When the emulator or the PTY cannot be created.
pub fn spawn<S, E, F>(
    spawner: S,
    spec: SpawnSpec,
    config: HolderConfig,
    log: LogContext,
    make_emulator: F,
) -> Result<SessionHandle, StartError>
where
    S: Spawn + Send + 'static,
    E: Emulator,
    F: FnOnce(Size) -> Result<E, E::Error> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let (events_tx, events) = mpsc::channel();
    let (ready_tx, ready) = mpsc::channel();
    let feed = tx.clone();
    let thread = thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let origin = Instant::now();
            let emulator = match make_emulator(spec.size) {
                Ok(emulator) => emulator,
                Err(e) => {
                    let _ = ready_tx.send(Err(StartError::Emulator(e.to_string())));
                    return;
                }
            };
            let mut pty = match spawner.spawn(&spec) {
                Ok(pty) => pty,
                Err(e) => {
                    let _ = ready_tx.send(Err(StartError::Pty(e)));
                    return;
                }
            };
            if let Err(e) = start_io(&mut pty, &feed) {
                let _ = ready_tx.send(Err(StartError::Thread(e)));
                return;
            }
            drop(feed);
            let _ = ready_tx.send(Ok(()));
            let mut actor = Actor {
                holder: Holder::new(config, spec.size),
                pty: Some(pty),
                emulator: Some(emulator),
                events: vec![events_tx],
                event_seq: 0,
                origin,
                log,
                queries: QueryScanner::default(),
                screen_harness: None,
                screen_at: None,
                #[cfg(test)]
                error_lines: Vec::new(),
            };
            actor.run(&rx);
        })
        .map_err(|e| StartError::Thread(e.to_string()))?;
    match ready.recv() {
        Ok(Ok(())) => Ok(SessionHandle {
            tx,
            events,
            thread: Some(thread),
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::Thread("ended before reporting".into())),
    }
}

/// Starts the reader and waiter threads. Each holds a sender, so neither outlives its use.
fn start_io<P: Pty>(pty: &mut P, feed: &Sender<Msg>) -> Result<(), String> {
    let mut reader = pty.reader().map_err(|e| e.to_string())?;
    let waiter = pty.take_waiter().ok_or("the PTY has no waiter")?;
    let out = feed.clone();
    thread::Builder::new()
        .name("session-read".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.send(Msg::Output(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = out.send(Msg::OutputClosed);
        })
        .map_err(|e| e.to_string())?;
    let exit = feed.clone();
    thread::Builder::new()
        .name("session-wait".into())
        .spawn(move || {
            let status = waiter.wait().unwrap_or(Exit {
                code: None,
                signal: None,
                raw: None,
            });
            let _ = exit.send(Msg::Exited(status));
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

struct Actor<P, E> {
    holder: Holder,
    pty: Option<P>,
    emulator: Option<E>,
    events: Vec<Sender<SequencedEvent>>,
    event_seq: u64,
    origin: Instant,
    log: LogContext,
    queries: QueryScanner,
    screen_harness: Option<Harness>,
    screen_at: Option<Duration>,
    #[cfg(test)]
    error_lines: Vec<String>,
}

impl<P: Pty, E: Emulator> Actor<P, E> {
    fn log_error(&mut self, operation: &str, error: &impl fmt::Display) {
        let line = self.log.error_line(operation, error);
        eprintln!("{line}");
        #[cfg(test)]
        self.error_lines.push(line);
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn run(&mut self, rx: &Receiver<Msg>) {
        let _ = self.step(Input::Spawned);
        while self.holder.phase() != Phase::Reaped {
            let deadline = [self.holder.deadline(), self.screen_at]
                .into_iter()
                .flatten()
                .min();
            let first = match deadline {
                Some(at) => rx.recv_timeout(at.saturating_sub(self.now())),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match first {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {
                    let _ = self.step(Input::Tick);
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
            for msg in rx.try_iter().take(BATCH) {
                self.handle(msg);
            }
            let _ = self.step(Input::Settle);
            if self.screen_at.is_some_and(|at| self.now() >= at) {
                self.observe_screen();
                self.screen_at = Some(self.now() + Duration::from_millis(300));
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        let input = match msg {
            Msg::Output(bytes) => Input::Output(bytes),
            Msg::OutputClosed => Input::OutputClosed,
            Msg::Exited(exit) => Input::ChildExited(exit),
            Msg::Command(command) => match command {
                Command::Subscribe(tx) => {
                    self.events.push(tx);
                    return;
                }
                Command::DetectScreen(harness) => {
                    self.screen_harness = harness;
                    self.screen_at = harness.map(|_| self.now() + Duration::from_millis(300));
                    return;
                }
                Command::Hook { harness, kind, seq } => Input::Hook(HookReport {
                    harness,
                    kind,
                    seq,
                    at: self.now(),
                }),
                Command::Attach(id, mode, size, budget, reply) => {
                    let result = self.step(Input::Attach {
                        viewer: id,
                        mode,
                        size,
                        budget,
                    });
                    let _ = reply.send(result);
                    return;
                }
                Command::AttachChannel(id, mode, size, budget, reply) => {
                    let result = self.step(Input::Attach {
                        viewer: id,
                        mode,
                        size,
                        budget,
                    });
                    let _ = reply
                        .send(result.map(|()| (self.holder.size(), self.holder.ring().next())));
                    return;
                }
                Command::Detach(id, reply) => {
                    let _ = reply.send(self.step(Input::Detach(id)));
                    return;
                }
                Command::Take(who, reply) => {
                    let _ = reply.send(self.step(Input::Take(who)));
                    return;
                }
                Command::ReleaseWriter(who, reply) => {
                    let _ = reply.send(self.step(Input::ReleaseWriter(who)));
                    return;
                }
                Command::ViewerResize(id, size, reply) => {
                    let _ = reply.send(self.step(Input::ViewerResize(id, size)));
                    return;
                }
                Command::Submit(who, bytes, reply) => {
                    let _ = reply.send(self.step(Input::Submit(who, bytes)));
                    return;
                }
                Command::ReadViewer(id, reply) => {
                    let _ = reply.send(self.holder.read_viewer(id));
                    return;
                }
                Command::ViewerSnapshot(id, reply) => {
                    let screen = self.emulator.as_ref().filter(|_| self.holder.has_screen());
                    let snapshot = screen.map(Emulator::serialize_vt).unwrap_or_default();
                    let next = self.holder.ring().next();
                    let _ = reply.send(self.holder.resynced(id).map(|()| (next, snapshot)));
                    return;
                }
                Command::ViewerGrid(id, reply) => {
                    let _ = reply.send(self.viewer_grid(id));
                    return;
                }
                Command::Interrupt(who, reply) => {
                    let _ = reply.send(self.step(Input::Interrupt(who)));
                    return;
                }
                Command::Signal(who, signal, reply) => {
                    let _ = reply.send(self.step(Input::SendSignal(who, signal)));
                    return;
                }
                Command::Close => Input::Close,
                Command::ReadFrom(seq, reply) => {
                    let _ = reply.send(self.holder.ring().read_from(seq));
                    return;
                }
                Command::Snapshot(reply) => {
                    let screen = self.emulator.as_ref().filter(|_| self.holder.has_screen());
                    let _ = reply.send(screen.map(Emulator::serialize_vt));
                    return;
                }
            },
        };
        let _ = self.step(input);
    }

    fn step(&mut self, input: Input) -> Result<(), Refusal> {
        let now = self.now();
        let mut result = Ok(());
        for effect in self.holder.step(input, now) {
            if let Effect::Refused(reason) = effect {
                result = Err(reason);
            } else {
                self.apply(effect);
            }
        }
        result
    }

    fn viewer_grid(&mut self, id: ViewerId) -> Result<Option<GridSnapshot>, Refusal> {
        let lost = matches!(self.holder.read_viewer(id)?, ViewerRead::Resync { .. });
        let screen = self.emulator.as_ref().filter(|_| self.holder.has_screen());
        let value = screen.map(|emulator| {
            (
                emulator.grid(),
                emulator.modes(),
                self.holder.ring().next(),
                lost,
            )
        });
        if value.is_some() {
            self.holder.resynced(id)?;
        }
        Ok(value)
    }

    fn observe_screen(&mut self) {
        let Some(harness) = self.screen_harness else {
            return;
        };
        let Some(emulator) = self.emulator.as_ref() else {
            return;
        };
        let detector = RuleDetector::default();
        if let Some(state) = detector.detect(harness, emulator.title().as_deref(), &emulator.grid())
        {
            let _ = self.step(Input::Screen(state));
        }
    }

    fn apply(&mut self, effect: Effect) {
        if let Some(event) = effect.viewer_event() {
            self.apply(Effect::Emit(event));
            return;
        }
        match effect {
            Effect::Feed(bytes) => {
                let mut start = 0;
                for (end, &byte) in bytes.iter().enumerate() {
                    if let Some(query) = self.queries.push(byte) {
                        if query == b"\x1b[6n" {
                            self.holder.expect_viewer_cpr();
                        }
                        let reply = self.emulator.as_mut().and_then(|emulator| {
                            let _ = emulator.feed(&bytes[start..=end]);
                            let cursor = if query == b"\x1b[6n" {
                                emulator.grid().cursor()
                            } else {
                                Cursor { row: 0, col: 0 }
                            };
                            PROFILE.reply(&query, emulator.size(), cursor)
                        });
                        if let Some(reply) = reply {
                            let _ = self.step(Input::Replies(reply));
                        }
                        start = end + 1;
                    }
                }
                if let Some(emulator) = self.emulator.as_mut() {
                    let _ = emulator.feed(&bytes[start..]);
                }
            }
            Effect::WritePty(bytes) => {
                if let Some(pty) = self.pty.as_mut()
                    && let Err(error) = pty.write(&bytes)
                {
                    self.log_error("write_pty", &error);
                }
            }
            Effect::ApplySize(size) => {
                if let Some(pty) = self.pty.as_mut()
                    && let Err(error) = pty.resize(size)
                {
                    self.log_error("resize_pty", &error);
                }
                if let Some(emulator) = self.emulator.as_mut()
                    && let Err(error) = emulator.resize(size)
                {
                    self.log_error("resize_emulator", &error);
                }
            }
            Effect::RedrawHint => {
                if let Some(pty) = self.pty.as_ref()
                    && let Err(error) = pty.redraw_hint()
                {
                    self.log_error("redraw_hint", &error);
                }
            }
            Effect::Interrupt => {
                if let Some(pty) = self.pty.as_mut()
                    && let Err(error) = pty.interrupt()
                {
                    self.log_error("interrupt", &error);
                }
            }
            Effect::Signal { signal, target } => {
                if let Some(pty) = self.pty.as_ref()
                    && let Err(error) = pty.signal(signal, target)
                {
                    self.log_error("signal", &error);
                }
            }
            Effect::Release => {
                self.emulator = None;
                self.pty = None;
            }
            Effect::Emit(event) => {
                self.event_seq += 1;
                let envelope = SequencedEvent {
                    seq: self.event_seq,
                    event,
                };
                eprintln!("{}", self.log.event_line(envelope));
                self.events
                    .retain(|subscriber| subscriber.send(envelope).is_ok());
            }
            Effect::Resync { viewer, oldest } => {
                self.apply(Effect::Emit(SessionEvent::ResyncRequired {
                    viewer,
                    oldest,
                }));
            }
            Effect::ViewerAttached(_) | Effect::ViewerDetached(_) | Effect::Refused(_) => {}
            // A variant added for a later run must be handled here in that run.
            _ => unreachable!("an effect this actor does not know: {effect:?}"),
        }
    }
}
