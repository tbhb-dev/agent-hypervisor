# Authorization model

Status: stub. Drafted in Phase 4 and stable after Phase 6 of the RFC-36 run plan.

This spec covers identities, scopes, read-only enforcement, and audit events.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Inside a Linux guest, `SO_PEERCRED` is fixed at connect, so in-guest attribution is advisory and the host authorizes on the workspace: [RFC-38 run 3, lines 16, 28, and 29](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r3-in-guest-api.md#L16).
- Credential and session expiry is checked on the host, because a resumed guest's clock ran up to 35 s behind: [RFC-38 run 10, lines 16 and 59](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
- An OS can't identify the process that wrote a byte to a socket. Attribution comes from the listener: [RFC-40 run 12, lines 16 and 36](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r12-ipc-sandbox-tiers.md#L16).
