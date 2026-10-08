---
paths: ["**/*.rs", "**/Cargo.toml", "Cargo.lock"]
---

Sources are the Rust 1.97 and 1.98 release notes and the Rust 2024 edition guide, read from the pinned 1.98.1 toolchain's `share/doc/rust/html/releases.md` and `share/doc/rust/html/edition-guide/rust-2024/`. Each rule ends with the release that introduced the behavior, or with the pinned version it was checked against when the rule is a repository practice.

## Toolchain and workspace

- Run cargo through mise tasks or `mise exec -- cargo`. mise pins the toolchain with the `default` profile, and the repository commits no `rust-toolchain.toml` (Rust 1.98.1).
- Keep `edition = "2024"`, `resolver = "3"`, and `rust-version = "1.98"` in `[workspace.package]`. Resolver 3 prefers dependency versions whose `rust-version` the workspace satisfies (Rust 2024).
- Spell `default-features`, `crate-type`, and `proc-macro` with hyphens. The edition rejects the underscore spellings (Rust 2024).
- Set `default-features` on the `[workspace.dependencies]` entry. A member that inherits a dependency with `workspace = true` cannot turn default features back off (Rust 2024).
- Pass `--locked` to every build, test, clippy, and run task so a stale `Cargo.lock` fails instead of changing (Cargo 1.98.1).

## Formatting and lints

- Format with the 2024 style edition, which sorts imports by version. `cargo fmt --all --check` runs in `check` (Rust 2024).
- Lint with `cargo clippy --workspace --all-targets --locked -- -D warnings`. The workspace turns on `clippy::all` and `clippy::pedantic` at warn. Silence one finding with `#[expect(clippy::name, reason = "...")]`, never a blanket allow (Rust 1.81).
- Give `#[unsafe(no_mangle)]` and `#[unsafe(export_name)]` the same local allow and reason as an `unsafe` block. The `unsafe_code` lint now fires for every unsafe attribute (Rust 1.98).
- Import names explicitly rather than through overlapping globs. More ambiguous glob imports are now hard errors (Rust 1.98).

## Unsafe and FFI

- Start every core crate with `#![forbid(unsafe_code, clippy::disallowed_methods, clippy::disallowed_types, clippy::disallowed_macros)]`. `check:boundary` fails a core crate root without it. A shell crate that needs unsafe allows `unsafe_code` on the smallest item and puts a `// SAFETY:` comment on every unsafe block (Rust 2024).
- Declare foreign functions in `unsafe extern "C"` blocks and mark each item `safe` or `unsafe` (Rust 2024).
- Wrap each unsafe operation inside an `unsafe fn` in its own `unsafe` block. The edition warns on `unsafe_op_in_unsafe_fn`, and the workspace denies it (Rust 2024).
- Set a child's environment through `Command::env`. `std::env::set_var` and `remove_var` are unsafe now (Rust 2024).
- Return `()` from bindings instead of `core::ffi::c_void`. The `c_void_returns` lint warns on it (Rust 1.98).
- Check bindgen wrappers before adding `repr(transparent)`. It no longer treats `repr(C)` fields, private fields, or `#[non_exhaustive]` types as trivial (Rust 1.98).

## Language changes to expect

- Drop a lock guard before an `if let ... else` that must not hold it. Temporaries in an `if let` scrutinee drop before the `else` block, and temporaries in a block's tail expression drop before the block's locals (Rust 2024).
- Narrow a return-position `impl Trait` with `use<..>` when a caller needs a shorter capture. By default it captures the lifetimes in scope, including ones the body never uses (Rust 2024).
- Name nothing `gen`. It is a reserved keyword (Rust 2024).
- Use atomics or `std::sync` types instead of references to `static mut`, which the edition denies (Rust 2024).
- Annotate the type where a diverging expression drives inference. The never type now falls back to `!` (Rust 2024).
- Bind a value to a local before comparing it with `assert_eq!` or `assert_ne!` if a later line borrows it. The macros now give their arguments a temporary scope (Rust 1.98).
- Derive `PartialOrd` and `Ord` together, or write both by hand. Deriving `Ord` now takes a fast path for `PartialOrd` (Rust 1.98).
- Update any test that matches symbol names or backtrace text. Rust uses v0 symbol mangling by default (Rust 1.97).

## Testing

- Put core invariants in `proptest!` blocks beside a unit test for each case, and turn a shrunk failure into a unit case (proptest 1.11.0).
- Keep core tests free of I/O, clocks, threads, and mocks. The crate's `clippy.toml` applies to its tests too (Rust 1.98.1).
- Put integration and conformance tests under a shell crate's `tests/`. The server conformance suite is `crates/hypervisord/tests/conformance.rs` (Rust 1.98.1).
