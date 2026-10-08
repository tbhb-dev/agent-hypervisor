# State reporting

Status: draft. Drafted in Phase 1 and stable after Phase 7 of the RFC-36 run plan.

This spec records the run 11 hook prototype with its screen fallback and session events and the Phase 1 conformance contract.

## Phase 1 conformance contract

A hook listener selects its session by socket path and normalizes only state-bearing fields. It passes source sequence and actor receive time to the holder, which ignores a non-increasing sequence. Each state change emits one `StateChanged` event. On child exit, that event precedes the lifecycle `Exited` event. A late subscriber receives future events only. Screen detection is disabled until explicitly enabled, and a visible blocker can correct a stale hook state. The first server-side cases exercise hooks and ordered events through the public session handle. The recorded hook and screen fixture tests cover detailed harness mappings, while live harness behavior remains untested.

The RFC-36 source at `tbhb-dev/agent-orchestration-poc.internal` commit `63ab6a891a2d167dfdf1faf6ec497b38996e93fa`, `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/source.md`, lines 117 to 120 and 340 to 347, names working, blocked, and idle hook events and a fallback trait. The merged run 11 code receives harness-specific hook names and derives those states. It does not accept a generic `working`/`blocked`/`idle` wire event. The draft follows the normalized hook schema in the code. The source names created, attached, detached, state-changed, and exited events. The merged stream adds running, resized, and reaped events and uses an in-memory per-session `u64` sequence with no durable cursor.

## Evidence and limits

Findings below are in `tbhb-dev/agent-orchestration-poc.internal` at commit `485c37e54cd69d186a4b5eb0909f5bdee822fe1b`.

- [RFC-36 run 3 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L103-L127) records Claude Code 2.1.293 and Codex CLI 0.157.1 against a fake model.
- [RFC-36 run 5 hook table](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r5-agy-capture.md#L93-L106) records agy 1.2.12 against a fake model.
- [RFC-40 run 18](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L74-L82) records herdr's screen fallback and its stale working history.

Run 11 replays the 15 recorded `hooks.jsonl` metadata shapes with their `event`, `fields`, `enums`, and timestamps, plus expected states and relative event times from the findings tables. The fixtures preserve hook metadata after prompt and tool input were removed. A live harness test remains open.

## Hook schema

Each accepted report has `harness` (`Claude`, `Codex`, or `Agy`), normalized `kind`, source `seq`, and a monotonic `at` supplied by the session actor. The hook client stamps `seq` from `CLOCK_MONOTONIC` nanoseconds before reading stdin or connecting. The listener forwards it without renumbering. A report whose sequence is not greater than the last accepted one is ignored, including a late report after a newer one. The client strips prompt text, tool arguments, transcript paths, and model names. It keeps the payload's `session_id` or `conversationId` only as `claimed_session_id` audit metadata beside the peer UID and GID.

The state values are `Unknown`, `Idle`, `Working`, `Blocked { reason: Approval | Input | Unknown }`, and `Exited`. The holder's own child exit sets `Exited`, even if a hook is silent. Later hook and screen observations leave that state unchanged.

| Harness | Hook event | Result |
| --- | --- | --- |
| Claude | SessionStart, Stop | Idle |
| Claude | UserPromptSubmit, MessageDisplay, PreToolUse | Working |
| Claude | PermissionRequest, Notification with `notification_type: permission_prompt` | Blocked, Approval |
| Claude | SessionEnd | Unknown; `/clear` and `/resume` can end a harness session while the child continues |
| Codex | SessionStart | No state change: it arrives at the first prompt, not at launch |
| Codex | UserPromptSubmit, PreToolUse | Working |
| Codex | PermissionRequest | Blocked, Approval |
| Codex | Stop, Interrupt | Idle |
| Codex | SessionEnd | Unknown until the holder observes child exit |
| agy | PreInvocation, PostInvocation | Working |
| agy | PreToolUse | Working, then Blocked, Unknown if no later event arrives within 500 ms |
| agy | PostToolUse | Working, including after an unanswered approval timeout |
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

A rule is `(harness, region, literal pattern, resulting state, priority)`. `Title` and `BottomNonEmpty(N)` are the prototype regions. The detector selects the matching rule with the highest priority. The starter rules include both observed Codex `Action Required` title variants, Claude's `Do you want to proceed?` approval UI, agy's `Run this command?` approval UI, the `esc to cancel` and `esc to interrupt` working footers, and the idle prompt text recorded in runs 3 and 5. A matched blocker can override hook state, while idle and working footer matches leave the hook state intact. Unmatched screens also leave the prior hook state unchanged.

The fixture for screen checkpoints records byte offsets at the end of each cell's idle and approval steps. Replaying those prefixes through Ghostty and alacritty detects Idle and Blocked, Approval in all 15 cells. The final grids are checked too: three Claude cells still display a permission question after the child exits, but the holder's Exited state ignores those screen observations. The checkpoint test validates only the recorded 120 by 40 screens. [Issue 33](https://github.com/tbhb-dev/agent-hypervisor/issues/33) tracks narrow panes and live prompt conformance, including the selection dialog in [herdr issue 2868](https://github.com/herdrdev/herdr/issues/2868).

| Recorded or historical case | Hooks alone | Screen path |
| --- | --- | --- |
| Main Stop followed by SubagentStop or PostToolUse | Idle: those later events do not revive Working | No override without a visible blocker |
| Claude Ctrl-C at a permission prompt | Blocked, Approval remains because run 3 did not observe an interrupt effect | A visible approval question confirms the block; [issue 35](https://github.com/tbhb-dev/agent-hypervisor/issues/35) tracks release after Ctrl-C |
| agy Ctrl-C at an approval prompt | Blocked, Unknown after the unanswered PreToolUse timeout; agy reported nothing on Ctrl-C | `Run this command?` upgrades the reason to Approval while visible; [issue 35](https://github.com/tbhb-dev/agent-hypervisor/issues/35) tracks release |
| Codex Ctrl-C at approval | Interrupt sets Idle | No override after the blocker disappears |
| Codex plan-mode input prompt after a working hook | Working remains stale if no Stop or other hook arrives | [Issue 33](https://github.com/tbhb-dev/agent-hypervisor/issues/33) tracks the missing recorded sequence and prompt rule |
| A hook source stops reporting after Working | Working remains stale | A visible blocker can correct it; [issue 35](https://github.com/tbhb-dev/agent-hypervisor/issues/35) tracks idle classification |

The subagent, post-tool, and plan-mode histories are from [RFC-40 run 18](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r18-herdr.md#L76-L80). The Ctrl-C outcomes come from runs 3 and 5. The table distinguishes tested replay sequences from screen behavior that still needs a live or narrow-pane conformance check.

## Session event stream

The holder emits `Created`, `StateChanged { from, to }`, and `Exited(Exit { code, signal, raw })` as plain values. Run 10's holder effects for viewer changes map to `Attached { viewer }` and `Detached { viewer }` events, including the `ViewerId`. The actor attaches a per-session `u64` sequence before sending each event to subscribers, preserving order across `Created`, viewer changes, the existing `Running` and `Resized` events, state transitions, exit, and `Reaped`. A subscriber added with `subscribe()` receives later events in that same order. Holder exit emits a state transition to Exited before the lifecycle Exited event.

The stream is local to the running actor. It does not include RFC-40's per-host event log, cursors, or push delivery. Event times and the agy deadline use the monotonic clock passed to the holder, following [RFC-38 run 10](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/485c37e54cd69d186a4b5eb0909f5bdee822fe1b/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
