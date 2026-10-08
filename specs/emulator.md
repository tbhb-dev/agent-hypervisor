# Emulator

Status: draft. Drafted in Phase 1 by RFC-36 run 7.

This spec covers the terminal emulator a session holder runs over its PTY output: the `Emulator` trait and its value types, snapshots, query replies, resize, the ghostty-vt build, thread ownership, the `alacritty_terminal` cross-check, and the recorded-session corpus. RFC-36 run 8 added the last two.

## Trait and value types

`hypervisor_core::emulator` defines the trait and its values with no I/O. `hypervisor-ghostty` provides the trait on ghostty-vt, and `hypervisor-alacritty` provides it on `alacritty_terminal` 0.26.0 as the fallback and cross-check.

- `feed(bytes) -> replies` parses PTY output and returns the bytes the terminal owes the application, in order, ready to write back to the PTY.
- `size()` and `resize(size)` read and set the grid size. `Size` rejects a zero dimension.
- `grid()` returns the active screen as a `Grid` of `Cell` values plus a `Cursor`.
- `modes()` returns the `Modes` in force.
- `serialize_vt()` returns VT bytes that redraw the active screen and restore the modes on a fresh terminal of the same size. Its default calls `serialize(grid, modes)`.

A `Cell` holds its grapheme as text, a foreground, background, and underline `Color`, eight `Attrs` flags, an `Underline` style, and a `CellWidth` of narrow, wide, spacer tail, or spacer head. `Modes` holds the active `Screen`, cursor visibility and keys, wraparound, `MouseTracking` and `MouseFormat`, alternate scroll, focus reporting, bracketed paste, synchronized output, mode 2027, and the pushed kitty keyboard flags. `diff(left, right)` lists the cells that differ between two grids.

## Snapshots

Viewers get snapshots from grid reads, never from ghostty-vt's VT formatter. The formatter restyles blank gaps and lost 189 cells on the `synthetic-alt` fixture and 1,677 on `codex-noalt-answered` (RFC-36 runs 2 and 3). `serialize` writes a full SGR at every style change. A blank cell is written with the style it has in the grid. The serializer loses 0 cells on all 20 recordings, in both emulators. It never emits mode 2026, and it writes mode 1007 both ways, because ghostty-vt sets 1007 by default and Codex resets it.

Server-side copies and restarts use ghostty-vt's binary snapshot codec instead, through `GhosttyEmulator::snapshot()` and `GhosttyEmulator::restore()`. A restore brings back the grid, cursor, modes, and query replies on every fixture.

## Query replies

The emulator answers queries while no viewer is attached. Replies come out of `feed`, not from a viewer path, so a detached session still gets its DA1 reply.

| Query | Reply |
| --- | --- |
| DA1 | `CSI ? 62;22 c` |
| DA2 | `CSI > 1;10;0 c` |
| XTVERSION | `agent-hypervisor` |
| CPR and DECRQM | from ghostty-vt's state |
| Kitty keyboard, `CSI ? u` | none |
| Kitty graphics | none, image storage is 0 |
| OSC 52 clipboard read | none, the callback is not installed |

Every reply passes `admit_reply`, which drops `CSI ? <digits> u`. Answering that query switches Claude Code to the kitty keyboard protocol and breaks Ctrl-C (RFC-36 run 3), so the emulator does not claim the protocol. Each of the 15 harness recordings gets a DA1 reply and no kitty keyboard reply.

## Resize

`resize` sets the grid size. The primary screen reflows and the alternate screen does not. Cell pixel sizes are passed as 0. Resizing each fixture replay to 80x24, 200x60, 1x1, and 120x40 did not crash (observed). Herdr's resize crash (herdr#4762) is untested here, because its input is not known.

A reply the terminal writes outside `feed`, such as the in-band size report for mode 2048 after a resize, waits until the next `feed` call. The holder must call `feed` after a resize, or run 9 changes `resize` to return replies. Run 10's capability profile decides whether mode 2048 is claimed.

## Ghostty build

`ghostty-vt-sys` builds libghostty-vt from Ghostty commit `a60e9e2a57f73e1eef2bd1cf2995a467f69e7fb0`, recorded in `crates/ghostty-vt-sys/ghostty.pin`, with Zig 0.16.0 pinned in `mise.toml`. `build.rs` takes the source from a shallow fetch of that commit, or from `git archive` on the clone named by `GHOSTTY_VT_SOURCE`, and git checks the hash either way. It runs `zig build -Demit-lib-vt -Doptimize=ReleaseFast -Dcpu=baseline -Demit-xcframework=false` and copies only `libghostty-vt.a` into a link directory of its own, because the Apple linker otherwise picks the sibling dylib and the binary fails to load (RFC-36 run 2). `otool -L` lists only `libSystem` as a shared library of the test binaries.

The bindgen 0.73.2 output is committed as `src/bindings.rs`, and `mise run ghostty:bindings` regenerates it. A pin move shows its API change as a diff, and builds don't need libclang. Move the Ghostty and Zig pins together.

## Thread ownership

`GhosttyEmulator` is neither `Send` nor `Sync`. ghostty-vt requires every call on a terminal to be serialized. The session holder creates one emulator per terminal on the session actor's thread and never moves it. A `compile_fail` doctest checks the missing `Send`.

This departs from the RFC-36 proposal and from run 2, which marked a terminal handle `Send` but not `Sync`. Run 9 can create the emulator on the session thread, so the `Send` marker is not needed. Herdr instead marks its handles `Send` behind a `Mutex` (RFC-40 run 18). [Session model](session-model.md) records the change.

## Cross-check with alacritty_terminal

`AlacrittyEmulator` wraps a `Term` and its `vte` processor. A second `vte` parser reads the same bytes and passes each CSI to `csi_effects` in the core, which answers DA1, DA2, DA3, and XTVERSION from `DEVICE_ATTRIBUTES` and `XTVERSION_NAME` and reports changes to modes 2026 and 2027. The input is cut after each such CSI, so replies keep the order of their queries. alacritty's own DA1 and DA2 replies (`CSI ? 6 c` and `CSI > 0;2600;1 c`) are dropped, and every reply passes `admit_reply`. The processor's synchronized-update timer never runs, so bytes inside mode 2026 apply as they arrive, as in ghostty-vt. The kitty keyboard stack is on, so pushed flags reach `Modes`, and its query reply is dropped.

Cells map one to one. Named colors 0 to 15 become palette indexes, and `NamedColor::Foreground`, `Background`, and the dim and bright variants become `Color::Default`, as spike 2 normalized them. These fields have gaps:

| Field | Gap |
| --- | --- |
| `Attrs::BLINK`, `Attrs::OVERLINE` | Never set. alacritty doesn't keep a flag for SGR 5 or 53 |
| `Modes::mouse_tracking` | Never `X10`. alacritty ignores mode 9 |
| `Modes::mouse_format` | Never `Urxvt` or `SgrPixels`. alacritty ignores modes 1015 and 1016 |
| `Modes::grapheme_clustering` | Tracked, but alacritty sizes each codepoint alone, so mode 2027 doesn't change any width |
| DECRQM for 1016 and 2027 | Answered as not recognized (`0`), where ghostty-vt answers reset (`2`) |

The golden tests clear exactly these fields from ghostty-vt's side before comparing.

## Golden tests

`crates/hypervisor-ghostty/tests/recordings.rs` replays every recording in the corpus through both emulators at its recorded size. It passes when:

- ghostty-vt's final text, cursor, and active screen match the recording's JSON;
- the count of differing cells between the two final grids equals `alacritty_grid_diffs`, and the cursors and modes agree. A failure lists the count and the first five differing cells;
- `serialize_vt` from each emulator redraws a fresh instance of the same emulator with 0 differing cells, the same cursor, and the same modes except 2026 and the kitty flags, and ghostty-vt's bytes redraw alacritty with `alacritty_grid_diffs` differences;
- both emulators return the same reply bytes for the whole recording, after the DECRQM differences above;
- each recording that used the alternate screen, replayed up to its last exit from it, gives the accepted counts after a shrink to 80 by 24 and a grow back to the recorded size.

A new difference fails the test, and an accepted one goes into the recording's JSON, or the test's resize table, with its cause.

## Known differences

| Cause | Where | Cells |
| --- | --- | --- |
| Regional-indicator width: ghostty-vt makes each indicator of a flag wide, alacritty narrow, and the rest of the row shifts | `synthetic-alt` row 5, the count spike 2 found | 30 |
| Alternate-screen shrink: ghostty-vt keeps the bottom rows, and alacritty keeps the top rows while the cursor fits. Growing back restores neither | `claude-fullscreen-answered`, `claude-fullscreen-ignored`, `synthetic-alt`, and `synthetic-exit`, at 80 by 24 and again at 120 by 40 | 1,166, 1,164, 693, 693 |

Every other recording gives 0 differing cells on the final screen and after the resize (observed). For run 14, a viewer redrawn after a smaller client attached sees different rows depending on the emulator, so the channel can't assume either policy. The other alternate-screen sessions give 0 differences at 80 by 24 because their screens are blank outside the rows both emulators keep.

## Corpus

The corpus is stored in `tests/fixtures/corpus/`: `recordings/<name>.vt` holds raw PTY output, and `expected/<name>.json` holds its metadata and final screen. The JSON records the file, its size and SHA-256, the terminal size, harness and version, screen mode and final screen, whether queries were answered, its source run and, for imported recordings, the repository, commit, and path, any redactions, the accepted grid difference count and cause, and the final cursor and text. Its `README.md` lists every field and each recording's provenance.

It holds 20 recordings at 120 by 40: nine Claude Code and Codex sessions from run 3 (`tbhb-dev/agent-orchestration-poc.internal`, `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture/cells/` at `485c37e5`), six `agy` sessions from run 5 (`findings/r5-agy-capture/cells/`), run 2's two synthetic streams (`findings/r2-emulator/fixtures/`), and run 8's `vim`, `htop`, and cold `cargo build` shell sessions. A new recording goes in the same two directories with a complete JSON file, and the golden tests pick it up by listing `expected/`.

## Known gaps

- The serializer assumes a fresh viewer, and a reused viewer keeps stale modes and the alternate screen: [#8](https://github.com/tbhb-dev/agent-hypervisor/issues/8), for run 14.
- `diff` compares only the area two grids share: [#9](https://github.com/tbhb-dev/agent-hypervisor/issues/9).
- The build stamp ignores the Zig version: [#10](https://github.com/tbhb-dev/agent-hypervisor/issues/10).
- Zig's global cache is outside the build directory, and a cold build fetches over the network: [#11](https://github.com/tbhb-dev/agent-hypervisor/issues/11).
- `serialize_vt` omits the primary screen behind an active alternate screen, scrollback, scroll regions, tab stops, charsets, hyperlinks, titles, palette changes, kitty flags, or a pending wrap. Run 14 decides which of these the channel needs.
- Grid reads take one FFI lookup per cell, and their cost at scale is untested.
- A cold build compiles libghostty-vt twice, once each for clippy and test.

## Evidence

Tests at Ghostty `a60e9e2a5`, Zig 0.16.0, `alacritty_terminal` 0.26.0, and rustc 1.98.1 on macOS arm64, with CI on Linux. The corpus and its provenance are in `tests/fixtures/corpus/`. The tests are in `crates/hypervisor-ghostty/tests/recordings.rs` and `tests/queries.rs`, which also checks that a grapheme longer than 16 codepoints reads back whole and that both emulators give the same identity replies, and in `crates/hypervisor-alacritty/tests/emulator.rs`.
