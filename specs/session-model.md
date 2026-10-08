# Session model

Status: stub. Drafted in Phase 1 and stable after Phase 5 of the RFC-36 run plan.

This spec covers session kinds, lifecycle, viewers, the write lock, size ownership, and ephemeral versus persistent sessions.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- One thread per session holds the PTY and emulator. Run 2 proposed a terminal handle that is `Send` but not `Sync`; run 7 made the emulator neither, created on the session actor's thread with one emulator per terminal, as [Emulator](emulator.md#thread-ownership) records: [RFC-36 run 2, lines 25 and 55](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L25).
- Herdr shares emulator handles behind a `Mutex` and has a resize crash that kills every pane: [RFC-40 run 18, lines 20 and 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L20).
- The holder owns terminal size as last-size-wins state; a same-size resize doesn't send `SIGWINCH`, and `portable-pty` cannot signal a process group: [RFC-40 run 11, lines 32, 33, and 35](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L32).
- Programmatic input takes the same write lock as a human: [RFC-40 run 18, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L22).

## Run 9 fragment

RFC-36 run 9 built the holder in three crates. `hypervisor_core::session` decides, `hypervisor-pty` is the PTY backend, and `hypervisor-session` runs one actor thread per session. Findings cited in this section are in the vault at commit `485c37e`.

### Session kinds and the spawn spec

A session is an `Agent` or a `Shell`. `SpawnSpec` holds the command, arguments, environment, working directory, an optional user, the initial size, and the kind. The environment is the child's whole environment, with nothing inherited from the daemon. `Debug` prints environment names and never values, because credential injection at spawn arrives in run 24 ([RFC-37 run 15, line 15](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2010Z-RFC-37-egress-credential-identity-spikes/findings/r15-model-keys.md#L15)).

Validation rejects an empty command, a NUL byte, an empty environment name or one containing `=`, and a relative working directory. A set `user` fails with `SpawnError::OtherUser { driver: "container" }`, because the host backend runs as the daemon's user and only run 20's driver can start a process as someone else.

### Persistence

Persistent sessions don't end when viewers leave. In an ephemeral session, the holder hangs up its process group a grace after its viewer count drops to zero, and only after the count was above zero once. The grace defaults to 0 here. RFC-36 question 14 proposes a grace equal to the resume window, and Phase 4 decides it ([proposal, line 244](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/proposal.md#L244)). The holder takes the viewer count as an input, because run 10 builds the viewer registry.

### Lifecycle

| State | Entered when | Leaves when |
| --- | --- | --- |
| `Starting` | the holder is created | the PTY and child are spawned |
| `Running` | the spawn succeeded | the child has exited and its output has closed, or the exit drain passed |
| `Exited { exit, reap_at }` | the child exited and its output closed, or the exit drain passed first | `reap_at`, which is `retain_exited` after entering `Exited` |
| `Reaped` | the retention passed | never; the emulator and PTY are dropped |

`Exit` keeps the exit code, the number of a killing signal, and the raw wait status. The holder waits up to `exit_drain` (100 ms) after the child exits for the PTY's output to close. The final screen then holds the child's last bytes. The actor answers snapshot requests through `Exited` and answers `None` once `Reaped`.

Closing is a hangup to the leader's process group, then a kill of the group after `kill_grace` (2 s). Both are deadlines in the holder, and no backend call blocks for a grace. A close's kill deadline is kept after the leader's exit and after the end of output. On macOS the kernel revokes the terminal when the session leader exits, and the master then reads its end even though a group member that ignored the hangup is still running (observed with `trap '' HUP` in the actor test). Closed output is not evidence of an empty group. The kill deadline sends `SIGKILL` to that member (verified by a core test and by the actor test). A reap kills the group first when the output is open or a close's kill is pending, which covers a `retain_exited` shorter than `kill_grace`. On Linux the output doesn't close while a member has the slave open, and that kill lets the actor's reader thread read the end and exit. A process that left the group with `setsid` and still has the slave open is not reached, and its reader thread and the master leak (untested).

Every timeout is a monotonic `Duration` that the actor reads with `Instant` and passes in. Wall time never enters: a resumed guest's clock stepped back by 35.3 s ([RFC-38 run 10, line 16](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16)). The defaults for retention, drain, and kill grace (60 s, 100 ms, and 2 s) are placeholders until Phase 4 sets the resume window.

### The step function

`Holder::step(input, now)` updates the state and returns effects in order. Inputs are spawn, PTY output, emulator replies, output closed, child exit, a resize request, a settle point, a viewer count, a write, an interrupt, a close, and a tick. Effects are `Feed`, `WritePty`, `ApplySize`, `RedrawHint`, `Interrupt`, `Signal`, `Release`, and `Emit`. Every deadline at or before `now` fires in the same step. The input, effect, and event enums are `#[non_exhaustive]` for runs 10 and 11.

PTY output goes into the ring and to the emulator. Run 10 added a query scanner to the actor. It passes output segments to `Emulator::feed` for screen state but discards the emulator's built-in replies. The fixed capability profile's replies pass through the holder in query order. A detached session answers DA1 this way. ConPTY sends its own DA1 and CPR queries and waits for the answers, which makes this a requirement on Windows ([RFC-40 run 11, line 31](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L31)). The holder drops replies and writes after the child has exited.

### Output ring

The ring holds recent output under a byte budget, 1 MiB by default. Sequence numbers count bytes, not chunks. Byte `n` of the session's output has sequence `n`, and `next()` equals the bytes ever appended. A viewer can resume at any byte offset, whatever chunks the reader thread happened to read. Eviction drops bytes from the front one at a time, and the retained bytes fill the budget exactly.

`read_from(seq)` returns the bytes from `seq` on. A sequence before the oldest retained byte answers `Gone { oldest }`, and the viewer needs a fresh snapshot. A sequence past the end answers `Ahead { next }`. `Gone` makes slow-viewer loss explicit, as [RFC-36 run 4, line 62](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r4-prior-art.md#L62) asks.

### Size ownership

The holder holds the size as last-size-wins state ([proposal, line 89](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/proposal.md#L89)). A resize request replaces any pending request. The actor handles up to 64 queued messages and then steps a settle point, and only then does the latest request take effect. A request for the size already applied becomes a redraw hint, `kill(-foreground, SIGWINCH)`, because the kernel doesn't signal a same-size `TIOCSWINSZ` ([RFC-40 run 11, line 32](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L32)). A real change resizes the PTY and the emulator, then feeds the emulator an empty slice, which flushes any reply queued during the resize, as [Emulator](emulator.md#resize) requires.

### Run 10 viewer and lock fragment

The viewer registry is holder-owned and takes opaque caller-supplied `ViewerId` values. `Attach` adds a viewer in `ReadOnly` or `ReadWrite` mode with its own size and nonzero budget for output bytes, without taking the lock. `Detach` removes it and releases its lock if held. The ephemeral lifecycle takes the registry count as input. Duplicate attachments and unknown detaches are refused. Run 10 emits holder effects `ViewerAttached(ViewerId)` and `ViewerDetached(ViewerId)` and tests both. Run 11's [PR #30](https://github.com/tbhb-dev/agent-hypervisor/pull/30) owns mapping those effects into session events and deciding whether the events contain `ViewerId`.

`Take` explicitly transfers the one lock to an attached viewer or a programmatic source. It promotes a read-only viewer to read-write. Input or focus never takes the lock. `ReleaseWriter` releases it. Viewer and programmatic writes and interrupts use the same check and return `NotWriter` or `UnknownViewer` without a PTY effect. This follows [RFC-36 proposal, lines 241 and 261](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/proposal.md#L241) and [RFC-40 run 18, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L22).

Each viewer remembers its requested size. Only the writer's size enters the holder's last-size-wins state. `Take` requests the new writer's remembered size. A same-size take requests a redraw hint. Read-only viewers' different sizes do not resize the PTY; rendering their letterbox is deferred to run 28, as [RFC-36 proposal, line 242](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/proposal.md#L242) proposes.

Output is queued per viewer under its byte budget. An output chunk that cannot fit clears that viewer's queue. It emits one `Resync { viewer, oldest }` effect with the output ring's oldest retained sequence and pauses live output for that viewer. Further output is withheld until the actor returns a grid snapshot with the ring's next sequence. The viewer then resumes live output. The caller reads a `Resync` notice until it requests the snapshot. No gap is silently delivered. This makes slow-viewer loss explicit as [RFC-36 run 4, line 62](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r4-prior-art.md#L62) asks. Delivery is pull-based in this prototype. Wire framing and resume belong to runs 13 to 15.

### One thread per session

The actor thread creates the emulator itself and runs the step function. The emulator and the PTY belong to that thread. A reader thread and a waiter thread send output and the exit over the same channel. Commands and events cross threads as plain values, and no emulator handle leaves the actor thread. `GhosttyEmulator` is neither `Send` nor `Sync`, which the actor doesn't need.

The emulator runs in the daemon's process. A ghostty-vt crash takes every session in that process with it, as herdr#4762 did to every herdr pane: a SIGSEGV in `Terminal.resize` after a pane entered the alternate screen ([RFC-40 run 18, line 70](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L70)). Run 9 resized a session on the alternate screen 301 times, cycling through 80x24, 1x1, 200x60, 40x10, 300x100, and 2x50 while the child printed. In each of ten runs at Ghostty `a60e9e2a5`, between 292 and 300 of the requests took effect and the session went on printing afterwards (observed). Herdr's crashing input is unknown and this test doesn't reproduce it, which leaves the crash possible here. A separate emulator process per session would contain it at the cost of a copy of every output byte, and the trade waits for the shim work in runs 17 and 18.

### PTY backend

`hypervisor-pty` replaces `portable-pty` 0.9.0 instead of wrapping it. That crate's `kill` sends `SIGHUP` to the child PID only, with no process group call. Its `ExitStatus` also reports a killing signal as `strsignal` text with code 1 (`pty/src/lib.rs` lines 210 to 238 and 341 to 372; [RFC-40 run 11, line 35](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L35)). The Unix backend opens the PTY with `posix_openpt` through `rustix` 1.1.5 and sets the size before exec. The child calls `setsid` and takes the slave as its controlling terminal with `TIOCSCTTY`. `Target::Group` signals the leader's process group and `Target::Foreground` the terminal's foreground group, while `Target::Leader` signals the leader alone. Closing uses the holder's signal deadlines, so the sketch's `close(grace)` is not a backend call.

`Caps` holds the rows where the hosts differ. The Unix backend sets the first three to true and the last two to empty and false.

| Capability | Unix | ConPTY, designed for and not built |
| --- | --- | --- |
| `foreground` | `tcgetpgrp` on the master | none |
| `signals` | `kill` on a PID or group | none |
| `redraw_hint` | `kill(-foreground, SIGWINCH)` | none |
| `backend_queries` | none | DA1, and CPR after a resize |
| `resize_repaints` | false | true, conhost repaints its buffer |

On Linux `openpt` sets `O_CLOEXEC` atomically. macOS `posix_openpt` has no such flag. There the backend sets `FD_CLOEXEC` right after it and serializes its own spawns with a process-wide lock. A fork from code outside the backend can still inherit the master in that window.

The tests run on macOS locally and on Linux in CI. A panic on the actor thread, such as the `unreachable!` for an effect it doesn't know, ends the session with no event: subscribers see only the event channel disconnect, and run 12's logs should record it.

### Run 9 evidence

`crates/hypervisor-pty/tests/pty.rs` and `crates/hypervisor-session/tests/actor.rs` run `/bin/sh` children. The PTY tests read the spawn size back with `stty size` and check exit codes and killing signal numbers. One sends a group signal to a shell with a background job. The actor tests check that a same-size resize sends only a redraw hint and that a burst of four resizes ends at the last size. Others check a DA1 query answered while detached and a read of an evicted sequence. The final screen is kept until the retention deadline and then reaped, and an ephemeral session ends when its viewers leave. In 30 runs the burst of four requests settled to one applied resize every time (observed).
