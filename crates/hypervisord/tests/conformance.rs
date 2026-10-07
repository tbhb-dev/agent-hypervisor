//! Server-side conformance suite (RFC-36 source line 315).
//!
//! Every runtime driver must pass this suite. It is empty until RFC-36 run 12 adds the first
//! cases: spawn, attach, detach, resume, read-only enforcement, write-lock handoff, and exit.
//! `cargo test --workspace` builds and runs this target with zero cases until then.
