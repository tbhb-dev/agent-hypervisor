# State reporting

Status: stub. Drafted in Phase 1 and stable after Phase 7 of the RFC-36 run plan.

This spec covers the hook event schema, the session state machine, and heuristic fallback rules.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Hooks covered working, blocked, and idle in Claude Code and Codex, and Codex also flags a blocked state in its window title: [RFC-36 run 3, line 21](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L21).
- Herdr dropped hook-based state after stale `working` bugs and reads the screen instead, so keep a screen path ready: [RFC-40 run 18, lines 20 and 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L20).
