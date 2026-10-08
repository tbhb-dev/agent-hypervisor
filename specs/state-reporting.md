# State reporting

Status: stub. Drafted in Phase 1 and stable after Phase 7 of the RFC-36 run plan.

This spec records the run 11 hook prototype with its screen fallback and session events. Run 12 drafts the conformance contract.

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

## Screen detection

Screen detection starts disabled. A caller enables it for a known harness with `Command::DetectScreen(Some(harness))` and disables it with `None`. When enabled, the actor samples after output batches and at least every 300 ms while quiet. `ScreenDetector` receives the emulator's grid and OSC title. This title accessor was added to the `Emulator` trait because run 3 recorded Codex's approval signal in its title; Ghostty exposes a borrowed title through `GHOSTTY_TERMINAL_DATA_TITLE`, while alacritty emits title events. The accessor copies the current value before the next terminal mutation.

A rule is `(harness, region, literal pattern, resulting state, priority)`. `Title` and `BottomNonEmpty(N)` are the prototype regions. The detector selects the matching rule with the highest priority. The starter rules include both observed Codex `Action Required` title variants, Claude's `Do you want to proceed?` approval UI, agy's `Run this command?` approval UI, the `esc to cancel` and `esc to interrupt` working footers, and the idle prompt text recorded in runs 3 and 5. A visible approval rule takes precedence over a working footer or an unconfirmed hook state. Unmatched screens leave the prior hook state unchanged, so run 12 must test more prompt shapes before enabling detection by default.

The fixture for screen checkpoints records byte offsets at the end of each cell's idle and approval steps. Replaying those prefixes through Ghostty and alacritty detects Idle and Blocked, Approval in all 15 cells. The final grids are checked too: three Claude cells still display a permission question after the child exits, but the holder's Exited state ignores those screen observations. The checkpoint test validates only the recorded 120 by 40 screens, not narrow panes or live harness releases. [Herdr issue 2868](https://github.com/herdrdev/herdr/issues/2868) describes a narrow Claude selection dialog detected as idle.

| Recorded or historical case | Hooks alone | Screen path |
| --- | --- | --- |
| Main Stop followed by SubagentStop or PostToolUse | Idle: those later events do not revive Working | Idle prompt confirms if visible |
| Claude Ctrl-C at a permission prompt | Blocked, Approval remains because run 3 did not observe an interrupt effect | Approval question confirms the visible block |
| agy Ctrl-C at an approval prompt | Blocked, Unknown after the unanswered PreToolUse timeout; agy reported nothing on Ctrl-C | `Run this command?` upgrades the reason to Approval while visible |
| Codex Ctrl-C at approval | Interrupt sets Idle | Idle prompt confirms after the dialog closes |
| Codex plan-mode input prompt after a working hook | Working remains stale if no Stop or other hook arrives | Needs a prompt rule; this recording set did not include plan mode |
| A hook source stops reporting after Working | Working remains stale | Known idle prompt rules can correct it; unmatched screens remain unclassified |

The subagent, post-tool, and plan-mode histories are from [RFC-40 run 18](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L76-L80). The Ctrl-C outcomes come from runs 3 and 5. The table distinguishes tested replay sequences from screen behavior that still needs a live or narrow-pane conformance check.

## Session event stream

The holder emits `Created`, `StateChanged { from, to }`, and `Exited(Exit { code, signal, raw })` as plain values. The actor attaches a per-session `u64` sequence before sending each event to subscribers, preserving order across `Created`, the existing `Running` and `Resized` events, state transitions, exit, and `Reaped`. A subscriber added with `subscribe()` receives later events in that same order. Holder exit emits a state transition to Exited before the lifecycle Exited event. `Attached` and `Detached` are defined in the event type. Run 10 owns viewer and lock wiring and will emit them if it merges second.

The stream is local to the running actor. It does not include RFC-40's per-host event log, cursors, or push delivery. Event times and the agy deadline use the monotonic clock passed to the holder, following [RFC-38 run 10](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
