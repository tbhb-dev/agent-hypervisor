# Guest agent protocol

Status: draft. Drafted in Phase 3 and stable after Phase 3 of the RFC-36 run plan.

This spec covers host-to-guest control and channel transport over a Unix socket and vsock.

## Run 20 CLI guest

The CLI prototype mounts a Linux arm64 `host-shim` executable as `/run/hypervisor/agent` and makes `guest /run/hypervisor/workload.json /run/hypervisor` its entrypoint. The image contains the tools and harnesses. The mounted binary runs the session holder. `mise run guest:build` cross-builds that binary with the pinned Rust toolchain, the musl target, Rust LLD, and the Alacritty emulator. The host and Seatbelt builds continue to use Ghostty.

The guest starts the existing shim control and terminal listeners inside its private `/run/hypervisor` directory. Its published `/run/hypervisor/agent.sock` accepts one JSON `GuestRequest` followed by a newline. `Control(HostRequest)` relays one existing shim request and returns one `HostResponse` JSON value. `Attach(path)` accepts only a terminal socket below `/run/hypervisor/r/` and then forwards the byte-framed terminal channel in both directions. The host creates a local private socket per session and forwards each viewer to that guest socket. Closing a viewer connection closes its relay. The guest session holder remains alive. On daemon restart, the host reconnects and checks the stable workload ID before listing held sessions and recreating local terminal proxies.

The CLI uses `--publish-socket <private-host-path>:/run/hypervisor/agent.sock`, documented by [Apple container 1.4.1](https://github.com/apple/container/blob/1.4.1/docs/command-reference.md#container-run). This is a proposed host-initiated proxy in the prototype. Run 1 verified a host socket mounted *into* a guest with `--volume`, which is a different direction. The host publishes only inside its 0700 runtime root. The guest control and terminal sockets have mode 0600, so ordinary guest session users cannot open them. The host's path and exact identity reply authorize the workload. A forwarded session name is audit data. The published socket's runtime behavior and restart semantics require `mise run check:host` on an unsandboxed Mac.

The guest agent runs as root and launches each session under an explicit numeric UID from 10000 through 60000. A live session cannot reuse another live session's UID. `reviewer:<uid>` selects a reviewer process with `GIT_OPTIONAL_LOCKS=0` and a private home in the cache volume. A Linux Landlock ruleset denies filesystem writes by default and allows writes beneath `/cache`, `/tmp`, and `/dev`. Creating or installing that ruleset must succeed before the reviewer process starts. The rule includes truncate, rename, and device ioctl write rights through Landlock ABI 5. Whether Apple container's guest kernel and mounted workspace honor these restrictions is untested until the host check runs.

The guest session actor creates and retains the PTY, emulator, byte history, and terminal socket. `container exec` does not start a session and is not part of this control path. The host may stop the whole VM with `container stop --time 0`. It does not wait for the agent. Container restart ends its earlier sessions. A guest agent upgrade applies on the next container start.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- The guest agent starts and holds every session process, because a killed `container exec` client orphans its guest process: [RFC-36 run 1, lines 19 and 20](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r1-apple-container.md#L19).
- Mount the host socket with `--volume` and its mode set before `container run`: [RFC-36 run 1, line 23](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r1-apple-container.md#L23).
- The host authorizes on the workspace and treats a forwarded session name as audit metadata: [RFC-38 run 3, lines 30, 31, and 38](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r3-in-guest-api.md#L30).
- The guest agent resyncs time from the host on every resume: [RFC-38 run 10, lines 16 and 59](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-38-command-and-control-spikes/findings/r10-pause-and-clocks.md#L16).
