//! The session holder's decisions (RFC-36 source lines 104 to 108, 112, and 113).
//!
//! A [`Holder`] is the state of one session. [`Holder::step`] takes one [`Input`] and the
//! monotonic time, updates the state, and returns the [`Effect`]s the shell must carry out, in
//! order. The shell owns the PTY, the emulator, the threads, and the clock; this module decides.
//! Time is a [`Duration`] since an origin the shell picks, read from a monotonic clock, never wall
//! time: a resumed guest's wall clock can step back (RFC-38 run 10).
//!
//! [`Input`], [`Effect`], and [`SessionEvent`] are `#[non_exhaustive]`, so the viewer registry
//! (run 10) and hook and screen state (run 11) can arrive as new variants of the same actor.

mod ring;
mod size;
mod spec;
mod viewer;

pub use ring::{OutputRing, RingRead};
pub use size::{Settled, SizeState};
pub use spec::{SessionKind, SpawnError, SpawnSpec};
pub use viewer::{Refusal, ViewerId, ViewerMode, ViewerRead, ViewerRegistry, Writer};

use std::num::NonZeroUsize;
use std::time::Duration;

use crate::emulator::Size;

/// How a child ended. `code` is set for a normal exit and `signal` for a killing signal; `raw` is
/// the backend's undecoded status, the `waitpid` status on Unix and the exit code on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Exit {
    /// The exit code.
    pub code: Option<i32>,
    /// The number of the signal that killed the child.
    pub signal: Option<i32>,
    /// The raw status.
    pub raw: Option<u32>,
}

/// A signal the holder can send. The backend maps it to the platform's number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signal {
    /// `SIGHUP`.
    Hangup,
    /// `SIGINT`.
    Interrupt,
    /// `SIGTERM`.
    Terminate,
    /// `SIGKILL`.
    Kill,
}

/// Which processes a signal reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The session leader alone.
    Leader,
    /// The session leader's process group, which its background jobs share.
    Group,
    /// The terminal's foreground process group.
    Foreground,
}

/// Whether a session outlives its viewers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persistence {
    /// Keeps running with no viewer attached.
    Persistent,
    /// Ends `grace` after its viewer count drops to zero. The count must have been above zero
    /// first, so a session spawned before its first viewer attaches is not ended at once.
    Ephemeral {
        /// How long a session with no viewer survives.
        grace: Duration,
    },
}

/// A holder's fixed settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HolderConfig {
    /// Whether the session outlives its viewers.
    pub persistence: Persistence,
    /// The output ring's byte budget.
    pub ring_budget: NonZeroUsize,
    /// How long the final state is kept after the child exits, before the session is reaped.
    pub retain_exited: Duration,
    /// How long to wait after the child exits for its output to close, so the final snapshot
    /// holds its last bytes. A background job that keeps the terminal open is cut off after it.
    pub exit_drain: Duration,
    /// How long after a hangup the process group is killed.
    pub kill_grace: Duration,
}

impl HolderConfig {
    /// The ring budget [`HolderConfig::new`] uses: 1 MiB.
    pub const DEFAULT_RING_BUDGET: NonZeroUsize = NonZeroUsize::new(1 << 20).unwrap();

    /// Defaults for `persistence`: a 1 MiB ring, the final state kept 60 s, a 100 ms exit
    /// drain, and a 2 s kill grace. These are placeholders until Phase 4 sets the resume window.
    #[must_use]
    pub const fn new(persistence: Persistence) -> Self {
        Self {
            persistence,
            ring_budget: Self::DEFAULT_RING_BUDGET,
            retain_exited: Duration::from_secs(60),
            exit_drain: Duration::from_millis(100),
            kill_grace: Duration::from_secs(2),
        }
    }

    /// An ephemeral session with a grace of 0. RFC-36 question 14 proposes a grace equal to the
    /// resume window; Phase 4 decides it.
    #[must_use]
    pub const fn ephemeral() -> Self {
        Self::new(Persistence::Ephemeral {
            grace: Duration::ZERO,
        })
    }
}

/// Where a session is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The PTY is not spawned yet.
    Starting,
    /// The child is running, or has exited while its output drains.
    Running,
    /// The child exited and its output closed, or the exit drain passed first. The final
    /// state is kept until `reap_at`, which counts `retain_exited` from entering this phase.
    Exited {
        /// How the child ended.
        exit: Exit,
        /// When the session is reaped.
        reap_at: Duration,
    },
    /// The session is gone and takes no more input.
    Reaped,
}

/// Something that happened to a session.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Input {
    /// The PTY and child were spawned.
    Spawned,
    /// The PTY produced output.
    Output(Vec<u8>),
    /// The capability profile's replies to the last [`Effect::Feed`].
    Replies(Vec<u8>),
    /// The PTY's output reached its end.
    OutputClosed,
    /// The child exited.
    ChildExited(Exit),
    /// The shell finished a batch of inputs; pending size requests settle now.
    Settle,
    /// The number of attached viewers changed.
    Viewers(usize),
    /// Attach a viewer without implicitly taking the write lock.
    Attach {
        /// Caller-supplied identity.
        viewer: ViewerId,
        /// Initial mode.
        mode: ViewerMode,
        /// Requested terminal size.
        size: Size,
        /// Maximum queued output bytes.
        budget: NonZeroUsize,
    },
    /// Detach a viewer, releasing its write lock if held.
    Detach(ViewerId),
    /// Explicitly take or transfer the write lock.
    Take(Writer),
    /// Release the write lock.
    ReleaseWriter(Writer),
    /// The attached viewer's requested size.
    ViewerResize(ViewerId, Size),
    /// Input from a viewer or programmatic source.
    Submit(Writer, Vec<u8>),
    /// Interrupt from the lock holder.
    Interrupt(Writer),
    /// End the session: hang up, then kill the group after the grace.
    Close,
    /// A deadline from [`Holder::deadline`] passed.
    Tick,
}

/// Something the shell must do, in the order returned.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Feed bytes to the emulator and answer queries from the capability profile.
    Feed(Vec<u8>),
    /// Write bytes to the PTY.
    WritePty(Vec<u8>),
    /// Resize the PTY and the emulator.
    ApplySize(Size),
    /// Ask the foreground group to redraw, as `kill(-pgrp, SIGWINCH)` does on Unix.
    RedrawHint,
    /// Interrupt the foreground job by writing the interrupt character.
    Interrupt,
    /// Send a signal.
    Signal {
        /// The signal.
        signal: Signal,
        /// Who receives it.
        target: Target,
    },
    /// Drop the emulator and the PTY.
    Release,
    /// Tell the session's subscribers.
    Emit(SessionEvent),
    /// A viewer attached; run 11 maps this to the session event stream.
    ViewerAttached(ViewerId),
    /// A viewer detached; run 11 maps this to the session event stream.
    ViewerDetached(ViewerId),
    /// The source's command was refused and had no PTY effect.
    Refused(Refusal),
    /// A viewer lost live output and must obtain a fresh grid snapshot.
    Resync { viewer: ViewerId, oldest: u64 },
}

/// What subscribers hear about a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionEvent {
    /// The child is running.
    Running,
    /// The session took a new size.
    Resized(Size),
    /// The child exited.
    Exited(Exit),
    /// The session is gone.
    Reaped,
}

/// The state of one session holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    config: HolderConfig,
    phase: Phase,
    ring: OutputRing,
    size: SizeState,
    viewers: usize,
    registry: ViewerRegistry,
    had_viewer: bool,
    closing: bool,
    output_closed: bool,
    pending_exit: Option<Exit>,
    end_at: Option<Duration>,
    kill_at: Option<Duration>,
    drain_until: Option<Duration>,
}

impl Holder {
    /// A holder in [`Phase::Starting`] for a PTY spawned at `size`.
    #[must_use]
    pub fn new(config: HolderConfig, size: Size) -> Self {
        Self {
            config,
            phase: Phase::Starting,
            ring: OutputRing::new(config.ring_budget),
            size: SizeState::new(size),
            viewers: 0,
            registry: ViewerRegistry::default(),
            had_viewer: false,
            closing: false,
            output_closed: false,
            pending_exit: None,
            end_at: None,
            kill_at: None,
            drain_until: None,
        }
    }

    /// The phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// The output ring.
    #[must_use]
    pub const fn ring(&self) -> &OutputRing {
        &self.ring
    }

    /// The attached viewers and write lock.
    #[must_use]
    pub const fn registry(&self) -> &ViewerRegistry {
        &self.registry
    }

    /// Pop a viewer's next bounded output item.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn read_viewer(&mut self, viewer: ViewerId) -> Result<ViewerRead, Refusal> {
        self.registry.read(viewer)
    }

    /// Mark a viewer live after the actor has made its fresh grid snapshot.
    ///
    /// # Errors
    ///
    /// If the viewer is not attached.
    pub fn resynced(&mut self, viewer: ViewerId) -> Result<(), Refusal> {
        self.registry.resynced(viewer)
    }

    /// The size applied to the PTY and the emulator.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size.applied()
    }

    /// Whether the emulator's state can still be read: true until the session is reaped.
    #[must_use]
    pub const fn has_screen(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Exited { .. })
    }

    /// The earliest pending deadline. The shell steps [`Input::Tick`] once it passes.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        let reap_at = match self.phase {
            Phase::Exited { reap_at, .. } => Some(reap_at),
            _ => None,
        };
        [
            self.end_at,
            self.kill_at,
            self.drain_until,
            reap_at,
            self.registry.input_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Applies `input` at monotonic time `now` and returns the effects to carry out, in order.
    /// Every deadline at or before `now` fires in the same step, so [`Holder::deadline`] is
    /// later than `now` afterwards.
    pub fn step(&mut self, input: Input, now: Duration) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.phase == Phase::Reaped {
            return fx;
        }
        let running = self.phase == Phase::Running;
        let accepts_input = running && self.pending_exit.is_none();
        match input {
            Input::Spawned => {
                if self.phase == Phase::Starting {
                    self.phase = Phase::Running;
                    fx.push(Effect::Emit(SessionEvent::Running));
                }
            }
            Input::Output(bytes) => {
                let seq = self.ring.next();
                self.ring.append(&bytes);
                for viewer in self.registry.output(seq, &bytes, self.ring.oldest()) {
                    fx.push(Effect::Resync {
                        viewer,
                        oldest: self.ring.oldest(),
                    });
                }
                fx.push(Effect::Feed(bytes));
            }
            Input::Replies(replies) => {
                if accepts_input && !replies.is_empty() {
                    fx.push(Effect::WritePty(replies));
                }
            }
            Input::OutputClosed => {
                self.output_closed = true;
                if let Some(exit) = self.pending_exit.take() {
                    self.exited(exit, now, &mut fx);
                }
            }
            Input::ChildExited(exit) => {
                if self.output_closed {
                    self.exited(exit, now, &mut fx);
                } else if self.pending_exit.is_none() {
                    self.pending_exit = Some(exit);
                    self.drain_until = Some(now + self.config.exit_drain);
                }
            }
            Input::Settle => self.settle(accepts_input, &mut fx),
            Input::Viewers(count) => self.viewers_changed(count, now),
            Input::Attach {
                viewer,
                mode,
                size,
                budget,
            } => match self.registry.attach(viewer, mode, size, budget) {
                Ok(()) => {
                    self.viewers_changed(self.registry.len(), now);
                    fx.push(Effect::ViewerAttached(viewer));
                }
                Err(reason) => fx.push(Effect::Refused(reason)),
            },
            Input::Detach(viewer) => match self.registry.detach(viewer) {
                Ok(()) => {
                    self.viewers_changed(self.registry.len(), now);
                    fx.push(Effect::ViewerDetached(viewer));
                }
                Err(reason) => fx.push(Effect::Refused(reason)),
            },
            Input::Take(who) => match self.registry.take(who) {
                Ok(Some(size)) if running => self.size.request(size),
                Ok(_) => {}
                Err(reason) => fx.push(Effect::Refused(reason)),
            },
            Input::ReleaseWriter(who) => {
                if let Err(reason) = self.registry.release(who) {
                    fx.push(Effect::Refused(reason));
                }
            }
            Input::ViewerResize(viewer, size) => match self.registry.resize(viewer, size) {
                Ok(true) if running => self.size.request(size),
                Ok(_) => {}
                Err(reason) => fx.push(Effect::Refused(reason)),
            },
            Input::Submit(who, bytes) => self.submit(who, &bytes, accepts_input, now, &mut fx),
            Input::Interrupt(who) => match self.registry.check_input(who) {
                Ok(()) if accepts_input => fx.push(Effect::Interrupt),
                Ok(()) => {}
                Err(reason) => fx.push(Effect::Refused(reason)),
            },
            Input::Close => self.close(now, &mut fx),
            Input::Tick => {}
        }
        self.fire_due(now, &mut fx);
        fx
    }

    fn settle(&mut self, accepts_input: bool, fx: &mut Vec<Effect>) {
        if !accepts_input {
            return;
        }
        match self.size.settle() {
            Settled::Idle => {}
            Settled::Unchanged => fx.push(Effect::RedrawHint),
            Settled::Apply(size) => {
                // The kernel sends SIGWINCH for a real change. The empty feed flushes replies
                // the emulator queued during the resize, such as a mode 2048 size report.
                fx.push(Effect::ApplySize(size));
                fx.push(Effect::Feed(Vec::new()));
                fx.push(Effect::Emit(SessionEvent::Resized(size)));
            }
        }
    }

    fn submit(
        &mut self,
        who: Writer,
        bytes: &[u8],
        accepts_input: bool,
        now: Duration,
        fx: &mut Vec<Effect>,
    ) {
        let bytes = match who {
            Writer::Viewer(id) => match self.registry.filter_input(id, bytes, now) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    fx.push(Effect::Refused(reason));
                    return;
                }
            },
            Writer::Program(_) => bytes.to_vec(),
        };
        if bytes.is_empty() {
            return;
        }
        match self.registry.check_input(who) {
            Ok(()) if accepts_input => fx.push(Effect::WritePty(bytes)),
            Ok(()) => {}
            Err(reason) => fx.push(Effect::Refused(reason)),
        }
    }

    fn viewers_changed(&mut self, count: usize, now: Duration) {
        self.viewers = count;
        if count > 0 {
            self.had_viewer = true;
            self.end_at = None;
            return;
        }
        if let Persistence::Ephemeral { grace } = self.config.persistence
            && self.had_viewer
            && self.phase == Phase::Running
            && !self.closing
            && self.end_at.is_none()
        {
            self.end_at = Some(now + grace);
        }
    }

    fn close(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        self.end_at = None;
        if self.phase != Phase::Running || self.closing || self.pending_exit.is_some() {
            return;
        }
        self.closing = true;
        fx.push(Effect::Signal {
            signal: Signal::Hangup,
            target: Target::Group,
        });
        self.kill_at = Some(now + self.config.kill_grace);
    }

    fn exited(&mut self, exit: Exit, now: Duration, fx: &mut Vec<Effect>) {
        self.phase = Phase::Exited {
            exit,
            reap_at: now + self.config.retain_exited,
        };
        self.end_at = None;
        self.drain_until = None;
        // A close in progress keeps its kill deadline for group members that ignored the
        // hangup. Closed output doesn't prove they are gone: macOS revokes the terminal when
        // the session leader exits, so the master reads its end while they still run.
        if !self.closing {
            self.kill_at = None;
        }
        fx.push(Effect::Emit(SessionEvent::Exited(exit)));
    }

    fn fire_due(&mut self, now: Duration, fx: &mut Vec<Effect>) {
        for (viewer, bytes) in self.registry.flush_due_input(now) {
            if self.phase == Phase::Running
                && self.pending_exit.is_none()
                && self.registry.check_input(Writer::Viewer(viewer)).is_ok()
            {
                fx.push(Effect::WritePty(bytes));
            }
        }
        let due = |at: Option<Duration>| at.is_some_and(|at| at <= now);
        if due(self.end_at) {
            self.close(now, fx);
        }
        if due(self.kill_at) {
            // Fires after the leader's exit too, for group members that ignored the hangup.
            self.kill_at = None;
            fx.push(Effect::Signal {
                signal: Signal::Kill,
                target: Target::Group,
            });
        }
        if due(self.drain_until) {
            self.drain_until = None;
            if let Some(exit) = self.pending_exit.take() {
                self.exited(exit, now, fx);
            }
        }
        if let Phase::Exited { reap_at, .. } = self.phase
            && reap_at <= now
        {
            self.phase = Phase::Reaped;
            let close_pending = self.kill_at.take().is_some();
            if !self.output_closed || close_pending {
                // A group member may still run: one holds the terminal, or a close hasn't sent
                // its kill yet. Kill the group so the slave closes, the PTY's reader sees the
                // end, and releasing the PTY frees it.
                fx.push(Effect::Signal {
                    signal: Signal::Kill,
                    target: Target::Group,
                });
            }
            fx.push(Effect::Release);
            fx.push(Effect::Emit(SessionEvent::Reaped));
        }
    }
}

#[cfg(test)]
mod tests;
