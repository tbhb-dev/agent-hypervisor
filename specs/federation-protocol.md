# Federation protocol

Status: stub. Drafted in Phase 6 and stable after Phase 6 of the RFC-36 run plan.

This spec covers peer identity, enrollment, link multiplexing, event subscription, and forwarding.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- The peer link uses the daemon's own Rust TLS stack, not Envoy, and Pingora is the egress broker base: [ADR-196](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/decisions/ADR-196-pingora-broker-base.md) and [RFC-37 run 1](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2010Z-RFC-37-egress-credential-identity-spikes/findings/r1-pingora.md).
