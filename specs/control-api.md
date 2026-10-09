# Control API

Status: stub. Drafted in Phase 4 and stable after Phase 6 of the RFC-36 run plan.

This spec covers workload and session operations, the event log with cursors, and idempotency.

## Run 23 prototype

Run 26 drafts this spec from the request and log formats that run 23 builds. It covers the daemon and proxy split, the session operations, the event log, and idempotency. Authorization is run 24.

`hypervisord daemon ROOT HOST SHIM LOG SOCKET` adopts the host driver's shims under `ROOT` and restores the host log from `LOG`. It then serves `SOCKET` with mode 0600. `hypervisor-proxy DAEMON_SOCKET LISTEN_SOCKET` is a separate process, and it doesn't open the driver root, the log, or the shim binary. A connection sends one newline-delimited JSON request. The proxy reads the client's peer UID and GID and attaches them as the `operator`. It then forwards the request and copies the daemon's reply lines back. A client request has only `request_id` and `request`, and the proxy refuses unknown fields, so a client cannot claim an operator. The proxy keeps running across a daemon restart because it connects per request.

Both processes run as the same user, and the daemon admits a connection only from its own UID. So the daemon trusts any same-user process that reaches its socket, and the split separates state and code paths, not privilege ([#119](https://github.com/tbhb-dev/agent-hypervisor/issues/119)). Whether the daemon should trust a proxy-attached identity at all is RFC-45's question, pending REQ-128 ([#74](https://github.com/tbhb-dev/agent-hypervisor/issues/74)).

The requests are `list_workloads`, `list_sessions`, `session_detail`, `spawn_session`, `kill_session`, and `watch`. Session detail joins the shim's live session list with the logged spawn. It reports the terminal socket and the spawning operator. Kill closes the session holder in the shim and waits for it to finish. Workload create, start, stop, and destroy are not on the API yet ([#87](https://github.com/tbhb-dev/agent-hypervisor/issues/87)).

The event log is per host and append-only. Each event has a cursor `{host, seq}` with `seq` counting from 1 without gaps, the request ID, a SHA-256 fingerprint of the request, the operator, and the change (`session_spawned` or `session_killed`). The daemon appends and fsyncs the line before it replies. On start it refuses a log with a malformed complete line, another host's event, a gap, or a repeated request ID, and truncates a torn final line that lacks its newline. Only control mutations are logged, and shim lifecycle events such as session exit are not ([#83](https://github.com/tbhb-dev/agent-hypervisor/issues/83)).

`watch` with `after` absent streams the whole log, and with a cursor streams every later event, then follows new appends until the client leaves. A subscriber resumes from the last cursor it saw, across daemon restarts, with no gap or repeat. A cursor from another host returns `foreign_cursor`, and one past the head returns `cursor_ahead`. The log has no retention, compaction, or epoch ([#121](https://github.com/tbhb-dev/agent-hypervisor/issues/121)).

Mutations need a nonempty client-supplied `request_id`, or the daemon returns `missing_request_id`. A repeated ID with the same fingerprint replays the logged outcome, including after a restart, without calling the driver again. A repeated ID with a different request returns `request_id_reused`. Failed requests are not logged, and a crash between the shim's side effect and the append loses the replay ([#120](https://github.com/tbhb-dev/agent-hypervisor/issues/120)).

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- No Phase 0 or RFC-37 to RFC-41 finding bears on this spec yet. Its inputs arrive with runs 23 and 26.
