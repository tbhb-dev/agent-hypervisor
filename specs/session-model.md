# Session model

Status: stub. Drafted in Phase 1 and stable after Phase 5 of the RFC-36 run plan.

This spec covers session kinds, lifecycle, viewers, the write lock, size ownership, and ephemeral versus persistent sessions.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- One thread per session holds the PTY and emulator. Run 2 proposed a terminal handle that is `Send` but not `Sync`; run 7 made the emulator neither, created on the session actor's thread with one emulator per terminal, as [Emulator](emulator.md#thread-ownership) records: [RFC-36 run 2, lines 25 and 55](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L25).
- Herdr shares emulator handles behind a `Mutex` and has a resize crash that kills every pane: [RFC-40 run 18, lines 20 and 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L20).
- The holder owns terminal size as last-size-wins state; a same-size resize doesn't send `SIGWINCH`, and `portable-pty` cannot signal a process group: [RFC-40 run 11, lines 32, 33, and 35](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r11-pty-supervision.md#L32).
- Programmatic input takes the same write lock as a human: [RFC-40 run 18, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L22).
