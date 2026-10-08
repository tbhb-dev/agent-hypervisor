//! Pure decisions behind the repository checks.
//!
//! The `xtask` shell crate gathers inputs (`cargo metadata` output, a commit message file) and
//! hands them here as plain values.
#![forbid(
    unsafe_code,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

pub mod boundary;
pub mod commit_msg;
