# Guest agent protocol

Status: stub. Drafted in Phase 3 and stable after Phase 3 of the RFC-36 run plan.

This spec covers host-to-guest control and channel transport over a Unix socket and vsock.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- The guest agent starts and holds every session process, because a killed `container exec` client orphans its guest process: [RFC-36 run 1, lines 19 and 20](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r1-apple-container.md#L19).
- Mount the host socket with `--volume` and its mode set before `container run`: [RFC-36 run 1, line 23](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r1-apple-container.md#L23).
- The host authorizes on the workspace and treats a forwarded session name as audit metadata: [RFC-38 run 3, lines 30, 31, and 38](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r3-in-guest-api.md#L30).
- The guest agent resyncs time from the host on every resume: [RFC-38 run 10, lines 16 and 59](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
