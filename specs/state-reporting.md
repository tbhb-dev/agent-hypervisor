# State reporting

Status: stub. Drafted in Phase 1 and stable after Phase 7 of the RFC-36 run plan.

This spec records the run 11 state mapping. Later stacked changes add the hook socket, screen fallback, and session events. Run 12 drafts the conformance contract.

## Evidence and limits

Findings below are in `tbhb-dev/agent-orchestration-poc.internal` at commit `485c37e54cd69d186a4b5eb0909f5bdee822fe1b`.

- [RFC-36 run 3 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L103-L127) records Claude Code 2.1.293 and Codex CLI 0.157.1 against a fake model.
- [RFC-36 run 5 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r5-agy-capture.md#L93-L106) records agy 1.2.12 against a fake model.
- [RFC-40 run 18](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L74-L82) records herdr's screen fallback and its stale working history.

The recorded event tables guide this state mapping. The stacked replay change tests each recorded cell without a live harness claim.

## Hook schema

Each accepted report has `harness` (`Claude`, `Codex`, or `Agy`), normalized `kind`, source `seq`, and a monotonic `at` supplied by the session actor. The socket listener allocates `seq` in accept order, so independent hook client invocations cannot reset it. A report whose sequence is not greater than the last accepted one is ignored. The planned listener strips prompt text, tool arguments, transcript paths, and model names. It keeps the payload's `session_id` or `conversationId` only as `claimed_session_id` audit metadata beside the peer UID and GID.

The state values are `Unknown`, `Idle`, `Working`, `Blocked { reason: Approval | Input | Unknown }`, and `Exited`. The holder's own child exit sets `Exited`, even if a hook is silent. Later hook and screen observations leave that state unchanged.

| Harness | Hook event | Result |
| --- | --- | --- |
| Claude | SessionStart, Stop | Idle |
| Claude | UserPromptSubmit, MessageDisplay, PreToolUse | Working |
| Claude | PermissionRequest, Notification with `notification_type: permission_prompt` | Blocked, Approval |
| Claude | SessionEnd | Exited |
| Codex | SessionStart | No state change: it arrives at the first prompt, not at launch |
| Codex | UserPromptSubmit, PreToolUse | Working |
| Codex | PermissionRequest | Blocked, Approval |
| Codex | Stop, Interrupt | Idle |
| Codex | SessionEnd | Exited |
| agy | PreInvocation, PostInvocation | Working |
| agy | PreToolUse | Working, then Blocked, Unknown if no later event arrives within 500 ms |
| agy | Stop with `fullyIdle: true` | Idle |

Other events do not change state. A later agy event cancels its pending PreToolUse deadline. The 500 ms timeout exceeds the recorded approximately 140 ms from PreToolUse to the approval prompt while limiting stale working. Run 12 must test this prototype threshold. A timeout does not distinguish an approval wait from another stalled tool call, so the reason is `Unknown`.
