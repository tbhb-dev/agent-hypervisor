//! Raw FFI to libghostty-vt.
//!
//! `build.rs` builds `libghostty-vt.a` with Zig from the Ghostty commit in `ghostty.pin` and links
//! it statically. `src/bindings.rs` is bindgen output over `ghostty/vt.h` at that commit, committed
//! so that builds need no libclang; regenerate it with `mise run ghostty:bindings` when the pin
//! moves. The header marks the whole API unstable.
//!
//! Every handle is a raw pointer, so it is neither `Send` nor `Sync`. The library creates no
//! threads, runs effect callbacks synchronously on the thread that writes, and requires the caller
//! to serialize every call on one terminal. This crate adds nothing on top: safe wrappers live in
//! `hypervisor-ghostty`.
#![allow(
    unsafe_code,
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    missing_docs,
    reason = "bindgen output mirrors the C names and declares foreign functions"
)]
#![allow(
    clippy::all,
    clippy::pedantic,
    reason = "generated code is not linted; regenerate it instead of editing"
)]

include!("bindings.rs");

/// The Ghostty commit the library and bindings come from.
pub const GHOSTTY_COMMIT: &str = include_str!("../ghostty.pin").trim_ascii();
