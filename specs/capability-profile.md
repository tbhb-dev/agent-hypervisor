# Capability profile

Status: stub. Drafted in Phase 1 and stable after Phase 8 of the RFC-36 run plan.

This spec covers the terminal modes and query responses the server advertises, and what each client must render.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Do not claim the kitty keyboard protocol. After the server answered the query, Claude Code switched to it and a raw `0x03` stopped acting as Ctrl-C ([RFC-36 run 3, line 26](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L26)).
- Regional-indicator width differs between emulators, 2 cells in ghostty-vt and 1 in `alacritty_terminal`: [RFC-36 run 2, line 20](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L20).
- Terminal query replies leaked into input in four herdr bugs. The profile's responder needs ordering tests: [RFC-40 run 18, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L22).
