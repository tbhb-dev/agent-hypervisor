//! The PTY backend a session holder drives (RFC-40 run 11's PTY interface sketch).
//!
//! [`Spawn`], [`Pty`], and [`Wait`] are capability-shaped, so a `ConPTY` backend can implement them
//! later and answer [`Support::Unsupported`] where Windows has no equivalent. [`UnixSpawner`] is
//! the only backend built. It opens the PTY with `rustix` directly instead of wrapping
//! `portable-pty` 0.9.0: that crate's `kill` sends `SIGHUP` to the child PID alone, it has no way
//! to signal a process group, and its `ExitStatus` keeps `strsignal` text with code 1 instead of
//! the signal number (`pty/src/lib.rs` lines 210 to 238 and 341 to 372).

mod unix;

#[cfg(target_os = "linux")]
pub use unix::GuestSpawner;
pub use unix::{UnixPty, UnixReader, UnixSpawner, UnixWaiter};

use std::fmt;
use std::io::{self, Read};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{Exit, Signal, SpawnError, SpawnSpec, Target};

/// What [`Pty::resize`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resize {
    /// The size changed; on Unix the kernel sends `SIGWINCH` to the foreground group.
    Applied,
    /// The PTY already had the size, so nothing was sent.
    Unchanged,
}

/// Whether the backend could do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// Done.
    Ok,
    /// The backend has no such operation, as `ConPTY` has no process groups.
    Unsupported,
}

/// What a backend can do. Each field is a row where the hosts differ (RFC-40 run 11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent capability"
)]
pub struct Caps {
    /// [`Pty::foreground`] can name the foreground process group. False on Windows.
    pub foreground: bool,
    /// [`Pty::signal`] can deliver signals. False on Windows.
    pub signals: bool,
    /// [`Pty::redraw_hint`] works. False on `ConPTY`, which has no `SIGWINCH`.
    pub redraw_hint: bool,
    /// The backend writes terminal queries of its own and waits for the answers, as conhost
    /// writes DA1 and CPR. Empty on Unix.
    pub backend_queries: &'static [&'static str],
    /// The backend repaints its own buffer after a resize, as conhost does.
    pub resize_repaints: bool,
}

/// Why a spawn failed.
#[derive(Debug)]
pub enum PtyError {
    /// The spec is invalid for this backend.
    Spec(SpawnError),
    /// The operating system refused.
    Io(io::Error),
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spec(e) => write!(f, "invalid spawn spec: {e}"),
            Self::Io(e) => write!(f, "spawn failed: {e}"),
        }
    }
}

impl std::error::Error for PtyError {}

impl From<io::Error> for PtyError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Starts a child on a new PTY.
pub trait Spawn {
    /// The PTY this backend returns.
    type Pty: Pty;

    /// Spawns `spec` with its size applied before the program starts.
    ///
    /// # Errors
    ///
    /// When the spec is invalid or the operating system refuses.
    fn spawn(&self, spec: &SpawnSpec) -> Result<Self::Pty, PtyError>;
}

/// Waits for the child, on a thread of its own.
pub trait Wait {
    /// Blocks until the child exits.
    ///
    /// # Errors
    ///
    /// When the status cannot be read.
    fn wait(self) -> io::Result<Exit>;
}

/// One PTY and its child. Closing is the holder's hangup and kill sequence through
/// [`Pty::signal`], so no call here blocks for a grace period.
pub trait Pty {
    /// Reads the PTY's output. A read returns 0 once the output has ended.
    type Reader: Read + Send + 'static;
    /// Waits for the child.
    type Waiter: Wait + Send + 'static;

    /// A reader for the output, to move to a reader thread.
    ///
    /// # Errors
    ///
    /// When the handle cannot be duplicated.
    fn reader(&self) -> io::Result<Self::Reader>;

    /// The waiter, once. Later calls return `None`.
    fn take_waiter(&mut self) -> Option<Self::Waiter>;

    /// Writes input for the application.
    ///
    /// # Errors
    ///
    /// When the write fails.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Sets the size.
    ///
    /// # Errors
    ///
    /// When the size cannot be read or set.
    fn resize(&mut self, size: Size) -> io::Result<Resize>;

    /// Writes the interrupt character, `0x03`, which every host turns into an interrupt.
    ///
    /// # Errors
    ///
    /// When the write fails.
    fn interrupt(&mut self) -> io::Result<()> {
        self.write(&[0x03])
    }

    /// Sends `signal` to `target`.
    ///
    /// # Errors
    ///
    /// When delivery fails, as it does once the target has exited.
    fn signal(&self, signal: Signal, target: Target) -> io::Result<Support>;

    /// Asks the foreground group to redraw without a size change.
    ///
    /// # Errors
    ///
    /// When delivery fails.
    fn redraw_hint(&self) -> io::Result<Support>;

    /// The foreground process group, when the backend has one.
    fn foreground(&self) -> Option<i32>;

    /// What the backend can do.
    fn caps(&self) -> Caps;
}
