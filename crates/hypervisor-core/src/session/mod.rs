//! The session holder's decisions (RFC-36 source lines 104 to 108, 112, and 113).
//!
//! The shell owns the PTY, the emulator, the threads, and the clock; this module decides. The
//! spawn spec says how a session starts, the output ring keeps its recent output, and the size
//! state owns its terminal size.

mod ring;
mod size;
mod spec;

pub use ring::{OutputRing, RingRead};
pub use size::{Settled, SizeState};
pub use spec::{SessionKind, SpawnError, SpawnSpec};
