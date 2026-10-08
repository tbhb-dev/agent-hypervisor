# Capability profile

Status: stub. Drafted in Phase 1 and stable after Phase 8 of the RFC-36 run plan.

This spec covers the terminal modes and query responses the server advertises, and what each client must render.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Do not claim the kitty keyboard protocol. After the server answered the query, Claude Code switched to it and a raw `0x03` stopped acting as Ctrl-C ([RFC-36 run 3, line 26](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L26)).
- Regional-indicator width differs between emulators, 2 cells in ghostty-vt and 1 in `alacritty_terminal`: [RFC-36 run 2, line 20](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L20).
- Terminal query replies leaked into input in four herdr bugs. The profile's responder needs ordering tests: [RFC-40 run 18, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L22).

## Run 10 profile fragment

`PROFILE` is one fixed value read by the emulator identity reply paths and by the session actor's query responder. The actor passes each complete query to the emulator to update screen state. It discards the emulator answer and writes the profile answer to the PTY before processing later output. A scanner keeps CSI and OSC sequences across PTY read boundaries. Replies follow query order even when an OSC 11 query precedes CPR, the ordering fault in [RFC-40 run 18, line 117](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L117). The child receives query replies while the actor is detached. ConPTY itself queries DA1 and CPR without a viewer ([RFC-40 run 11, line 31](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L31)).

Here `ESC`, `CSI`, `OSC`, `DCS`, `BEL`, and `ST` name their usual escape bytes. CPR uses the current one-based cursor. Size reports use current rows and columns with a fixed 16 by 8 pixel cell. The 80 by 24 examples below match the fixed profile in [RFC-36 run 3, line 26](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L26).

| Query bytes | Answer bytes at 80 by 24 or silence | Evidence |
| --- | --- | --- |
| `CSI c` | `CSI ? 62 ; 22 c` | Run 3 conservative DA1 |
| `CSI > c` | `CSI > 1 ; 10 ; 0 c` | Run 3 conservative DA2 |
| `CSI = c` | `DCS ! | 00000000 ST` | Run 3 conservative DA3 |
| `CSI > q` | `DCS > | agent-hypervisor ST` | Run 7 identity, shared profile |
| `CSI ? 2026 $ p` | `CSI ? 2026 ; 1 $ y` | Runs 3 and 5 synchronized output; `1` reports set |
| `CSI ? 2027 $ p` | `CSI ? 2027 ; 0 $ y` | Run 5 unsupported mode |
| `CSI ? n $ p` for other modes | `CSI ? n ; 2 $ y` for the fixed supported set, `0` otherwise | Run 3 conservative set |
| `CSI 6 n` | `CSI row ; col R` | Run 3 Codex batch |
| `CSI 5 n` | `CSI 0 n` | Run 3 conservative status reply |
| `OSC 10 ; ? ST`, `OSC 11 ; ? ST` | `OSC 10 ; rgb:d0d0/d0d0/d0d0 ST`, `OSC 11 ; rgb:1c1c/1c1c/1c1c ST` | Run 3 Codex batch |
| `CSI 14 t`, `CSI 16 t`, `CSI 18 t` | `CSI 4 ; 384 ; 640 t`, `CSI 6 ; 16 ; 8 t`, `CSI 8 ; 24 ; 80 t` | Run 3 conservative size reports |
| `CSI ? u`, kitty graphics `APC G ... ST` | silence | Run 3 Ctrl-C finding and run 5 query capture |
| `OSC 4 ; index ; ?`, `OSC 12 ; ?`, `DCS + q ... ST`, `DCS $ q ... ST` | silence | Outside this profile's fixed answers |
| Other terminal queries | silence | Untested beyond this fixed profile |

For OSC 10 and 11, a `BEL` query receives a `BEL` answer and an `ST` query receives an `ST` answer.

The fixed supported DEC mode set is `1, 7, 12, 25, 47, 1000, 1002, 1003, 1004, 1006, 1047, 1048, 1049, 2004, 2026`. The responder reports `1` (set) for 2026, `2` (reset) for the other listed modes, and `0` (not recognized) for others. Run 3's capture tool answered `2` for 2026, but this profile reports it as set because the holder supports synchronized output. The answer is fixed. It does not track mode state: an application that sets 2004 and then queries it still receives reset. The child may still push kitty flags. The profile leaves the kitty keyboard query unanswered, following [RFC-36 run 3, line 26](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L26).

Viewer input drops terminal-generated DA1, DA2, DA3, XTVERSION, CPR, DECRPM, OSC 4, OSC 10, OSC 11, and kitty keyboard answers before checking the write lock. `CSI 1 ; 2..8 R` also encodes modified F3. The filter forwards it unless a CPR query was sent to that viewer and awaits one answer. A CPR answer clears that expectation. If the terminal does not answer a query, the next modified F3 may be dropped. The prototype retains that ambiguity. The filter keeps incomplete escape sequences across viewer writes and discards a split reply before PTY input. It flushes an unfinished sequence after 50 ms so a standalone Escape key is delivered. A reply split by more than 50 ms can still pass through. Ordinary input needs the viewer's lock. This follows the reply leaks in [RFC-40 run 18, lines 116 to 119](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L116).

The holder answers immediately before raw mode and accepts the line discipline's echo. In [RFC-36 run 5, line 19](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r5-agy-capture.md#L19), agy sends DA1 before switching its TTY to raw mode, and all four answered cells show literal `^[[?62;22c`. Run 10 read `findings/r5-agy-capture/cells/agy-alt-answered/output.raw` at vault commit `485c37e5` and found that literal once at byte offset 38. A delayed reply would need a terminal-mode check and a deadline. Immediate replies satisfy Codex's 250 ms probe. Claude also sends its second query stage. [RFC-36 run 3, lines 18 and 19](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L18) records this.
