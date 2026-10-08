# State reporting

Status: stub. Drafted in Phase 1 and stable after Phase 7 of the RFC-36 run plan.

This spec records the run 11 hook prototype. Later stacked changes add screen fallback and session events. Run 12 drafts the conformance contract.

## Evidence and limits

Findings below are in `tbhb-dev/agent-orchestration-poc.internal` at commit `485c37e54cd69d186a4b5eb0909f5bdee822fe1b`.

- [RFC-36 run 3 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L103-L127) records Claude Code 2.1.293 and Codex CLI 0.157.1 against a fake model.
- [RFC-36 run 5 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r5-agy-capture.md#L93-L106) records agy 1.2.12 against a fake model.
- [RFC-40 run 18](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L74-L82) records herdr's screen fallback and its stale working history.

Run 11 replays fixtures containing only event names, `notification_type`, `fullyIdle`, relative event times, and the states expected from those tables, without prompt or tool input and without a live harness claim.

## Hook schema

Each accepted report has `harness` (`Claude`, `Codex`, or `Agy`), normalized `kind`, source `seq`, and a monotonic `at` supplied by the session actor. The socket listener allocates `seq` in accept order, so independent hook client invocations cannot reset it. A report whose sequence is not greater than the last accepted one is ignored. The listener strips prompt text, tool arguments, transcript paths, and model names. It keeps the payload's `session_id` or `conversationId` only as `claimed_session_id` audit metadata beside the peer UID and GID.

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

## Socket identity and hook client

`HookSocket::bind(root, workspace_id, session_id)` creates `root/r/<first 12 SHA-256 hex characters of workspace ID>/<first 12 SHA-256 hex characters of session ID>.s`. The workspace directory has mode `0700`, and binding fails if the full path exceeds 103 bytes. The listener path selects the session. Payload claims have no routing effect. Peer credentials are audit fields, not proof of the process that wrote each byte. These choices follow [RFC-40 run 12](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r12-ipc-sandbox-tiers.md#L16-L40), [RFC-39 run 3](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-39-filesystem-spikes/findings/r3-managed-directories.md#L40-L47), and [RFC-38 run 3](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r3-in-guest-api.md#L28-L38).

`hypervisord hook <claude|codex|agy> <socket> [agy-event]` reads up to 64 KiB of hook JSON from stdin. It forwards only state-bearing fields and exits zero if the listener is missing or slow. Socket work has a 150 ms client deadline and no error output. For agy `PreToolUse`, it prints `{"decision":"ask"}` even if the listener is unavailable because the recorded empty reply denied the tool call. Run 21 owns harness configuration installation.

These are configuration shapes for run 21 to install, with the binary and socket path replaced by the image's actual paths. They are derived from the run 3 and run 5 capture scripts, and have not been installed or live-tested in run 11.

Claude Code `--settings` JSON:

```json
{
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "PreToolUse": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "Notification": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}],
    "SessionEnd": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook claude /short/session.s", "timeout": 1}]}]
  }
}
```

Codex `hooks.json` in the invocation's `CODEX_HOME`:

```json
{
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "PreToolUse": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "Interrupt": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}],
    "SessionEnd": [{"hooks": [{"type": "command", "command": "/path/hypervisord hook codex /short/session.s", "timeout": 1}]}]
  }
}
```

agy `~/.gemini/config/hooks.json`:

```json
{
  "hypervisor-state": {
    "PreInvocation": [{"type": "command", "command": "/path/hypervisord hook agy /short/session.s PreInvocation", "timeout": 1}],
    "PostInvocation": [{"type": "command", "command": "/path/hypervisord hook agy /short/session.s PostInvocation", "timeout": 1}],
    "PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "/path/hypervisord hook agy /short/session.s PreToolUse", "timeout": 1}]}],
    "PostToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "/path/hypervisord hook agy /short/session.s PostToolUse", "timeout": 1}]}],
    "Stop": [{"type": "command", "command": "/path/hypervisord hook agy /short/session.s Stop", "timeout": 1}]
  }
}
```
