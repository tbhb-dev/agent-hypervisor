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
//! calls the backend. Errors from writes and signals are dropped until run 12 adds logs.

use std::fmt;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hypervisor_core::emulator::{Emulator, Size};
use hypervisor_core::session::{
    Effect, Exit, Holder, HolderConfig, Input, Phase, RingRead, SessionEvent, SpawnSpec,
};
use hypervisor_pty::{Pty, PtyError, Spawn, Wait};

/// The most messages handled before pending size requests settle.
const BATCH: usize = 64;

/// A request to a running session.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// A viewer asked for a size.
    Resize(Size),
    /// The number of attached viewers changed.
    Viewers(usize),
    /// Input for the application.
    Write(Vec<u8>),
    /// Interrupt the foreground job.
    Interrupt,
    /// End the session.
    Close,
    /// Read the output ring from a sequence number.
    ReadFrom(u64, Sender<RingRead>),
    /// The screen as VT bytes, or `None` once the session is reaped.
    Snapshot(Sender<Option<Vec<u8>>>),
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

/// The caller's side of a session. Dropping it closes the session.
pub struct SessionHandle {
    tx: Sender<Msg>,
    events: Receiver<SessionEvent>,
    thread: Option<JoinHandle<()>>,
}

impl SessionHandle {
    /// Sends a command. A command sent after the session thread has ended is dropped; the
    /// event channel's disconnection reports that end.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Command(command));
    }

    /// The session's events, in order. The channel disconnects when the session is reaped.
    #[must_use]
    pub const fn events(&self) -> &Receiver<SessionEvent> {
        &self.events
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
                events: events_tx,
                origin,
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
    events: Sender<SessionEvent>,
    origin: Instant,
}

impl<P: Pty, E: Emulator> Actor<P, E> {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn run(&mut self, rx: &Receiver<Msg>) {
        self.step(Input::Spawned);
        while self.holder.phase() != Phase::Reaped {
            let first = match self.holder.deadline() {
                Some(at) => rx.recv_timeout(at.saturating_sub(self.now())),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match first {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => self.step(Input::Tick),
                Err(RecvTimeoutError::Disconnected) => return,
            }
            for msg in rx.try_iter().take(BATCH) {
                self.handle(msg);
            }
            self.step(Input::Settle);
        }
    }

    fn handle(&mut self, msg: Msg) {
        let input = match msg {
            Msg::Output(bytes) => Input::Output(bytes),
            Msg::OutputClosed => Input::OutputClosed,
            Msg::Exited(exit) => Input::ChildExited(exit),
            Msg::Command(command) => match command {
                Command::Resize(size) => Input::Resize(size),
                Command::Viewers(count) => Input::Viewers(count),
                Command::Write(bytes) => Input::Write(bytes),
                Command::Interrupt => Input::Interrupt,
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
        self.step(input);
    }

    fn step(&mut self, input: Input) {
        let now = self.now();
        for effect in self.holder.step(input, now) {
            self.apply(effect);
        }
    }

    fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::Feed(bytes) => {
                if let Some(emulator) = self.emulator.as_mut() {
                    let replies = emulator.feed(&bytes);
                    self.step(Input::Replies(replies));
                }
            }
            Effect::WritePty(bytes) => {
                if let Some(pty) = self.pty.as_mut() {
                    let _ = pty.write(&bytes);
                }
            }
            Effect::ApplySize(size) => {
                if let Some(pty) = self.pty.as_mut() {
                    let _ = pty.resize(size);
                }
                if let Some(emulator) = self.emulator.as_mut() {
                    let _ = emulator.resize(size);
                }
            }
            Effect::RedrawHint => {
                if let Some(pty) = self.pty.as_ref() {
                    let _ = pty.redraw_hint();
                }
            }
            Effect::Interrupt => {
                if let Some(pty) = self.pty.as_mut() {
                    let _ = pty.interrupt();
                }
            }
            Effect::Signal { signal, target } => {
                if let Some(pty) = self.pty.as_ref() {
                    let _ = pty.signal(signal, target);
                }
            }
            Effect::Release => {
                self.emulator = None;
                self.pty = None;
            }
            Effect::Emit(event) => {
                let _ = self.events.send(event);
            }
            // A variant added for a later run must be handled here in that run.
            _ => unreachable!("an effect this actor does not know: {effect:?}"),
        }
    }
}
