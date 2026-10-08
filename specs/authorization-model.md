# Authorization model

Status: stub. Drafted in Phase 4 and stable after Phase 6 of the RFC-36 run plan.

This spec covers identities, scopes, read-only enforcement, and audit events.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Inside a Linux guest, `SO_PEERCRED` is fixed at connect, so in-guest attribution is advisory and the host authorizes on the workspace: [RFC-38 run 3, lines 16, 28, and 29](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r3-in-guest-api.md#L16).
- Credential and session expiry is checked on the host, because a resumed guest's clock ran up to 35 s behind: [RFC-38 run 10, lines 16 and 59](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
- An OS can't identify the process that wrote a byte to a socket. Attribution comes from the listener: [RFC-40 run 12, lines 16 and 36](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r12-ipc-sandbox-tiers.md#L16).

## Attenuation rules

These rules compute and check the grant a workload may give a child it creates. They are pure functions in `hypervisor_core::attenuation`, with a unit test for each of RFC-38 run 20's 22 request cases and a property test for each rule. Run 20 chose a grant table held by `agentd` as the provisional representation. The rules work over grant rows keyed by SPIFFE ID: [RFC-38 run 20, lines 18, 27, 28, and 43](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r20-grant-representation.md#L18).

Each grant contains an isolation floor, egress entries, credential references for the broker, and mounts. Isolation is ordered container above seatbelt above unsandboxed. Every egress entry specifies an exact host, with an optional method set and an optional path prefix. Mount entries specify a mount point, a mode where `rw` implies `ro`, and the scope below it that the holder may touch.

1. **The child is the intersection.** A child's row is `intersect(parent, requested)`. On every axis it allows only what both the parent and the request allow. A requested entry that one parent entry covers is kept as asked. Any other requested entry is replaced by its overlap with each parent entry.
2. **Isolation never weakens.** The child's isolation floor is the stronger of the parent's and the requested one. A spawn request is allowed only at or above the floor, so a container workload cannot create a seatbelt or unsandboxed one.
3. **Narrowing is reported, not silent.** `intersect` returns every item it removed or narrowed. Isolation can be raised. An egress entry can be removed, or narrowed in its methods or path. Credentials and mounts can be removed, and a mount can drop from `rw` to `ro` or lose part of its scope. The list is empty exactly when the request was already within the parent. The spawn path can then refuse a request that would be narrowed.
4. **Containment fails closed.** `within(grant, request)` denies an unknown host, mount point, or reference. Paths and prefixes match by whole segments, so `/workspace/src-evil/` is not below `/workspace/src/`. A path that is not absolute, or has an empty, `.`, or `..` segment, is denied.
5. **Credentials are references, never values.** A grant holds only `broker:` references. The child gets a reference only if the parent's grant holds it. The broker receives a policy grant made of the child's SPIFFE ID and a reference. No type in the model has a field for a secret.

Not covered yet: host name normalization (hosts compare as exact strings), percent-encoded paths, and overlap across several parent entries, where a request covered only by the union of two entries counts as narrowed. The grant table and its chain mode check follow in a stacked change. Lineage budgets, cross-host rows, and the authorization path that calls these functions are also out of scope.
