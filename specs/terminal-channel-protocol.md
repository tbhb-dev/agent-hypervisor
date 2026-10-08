# Terminal channel protocol

Status: stub. Drafted in Phase 2 and stable after Phase 7 of the RFC-36 run plan.

This spec covers framing, handshake, byte and grid encodings, resume, control frames, and events.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Serialize to viewers from grid reads, not the VT formatter, which restyles blank gaps: [RFC-36 run 2, lines 19 and 67](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L19) and [RFC-36 run 3, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L22).
- Specify mode-aware wheel and page-key routing, and decide which encoding carries scrollback: [RFC-36 run 4, lines 17 and 61](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r4-prior-art.md#L17).
- Attribution comes from the listener, with one socket per workspace or session and peer credentials kept as audit fields: [RFC-40 run 12, lines 16 and 36](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r12-ipc-sandbox-tiers.md#L16).
- Socket paths stay under the 104-byte `sun_path` limit, outside the workspace tree: [RFC-39 run 3, line 13](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-39-filesystem-spikes/findings/r3-managed-directories.md#L13).
