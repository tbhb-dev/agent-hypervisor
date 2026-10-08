# Terminal channel protocol

Status: draft. Drafted in Phase 2 and stable after Phase 7 of the RFC-36 run plan.

This spec covers framing, handshake, byte and grid encodings, resume, control frames, and events.

## Run 13 framing and handshake

The run 13 channel frame is a four-byte unsigned big-endian length followed by a three-byte header and a payload. The length counts the header and payload, not its own prefix, and must be between 3 and 1,048,576 inclusive. Header bytes 0 to 1 are the unsigned big-endian protocol version. Byte 2 is the frame type. A stream decoder waits for a full frame and consumes exactly its stated length. It leaves subsequent bytes for the next decode. It rejects an oversized or short length before allocating a payload. Unix sockets, WebSocket binary messages, and multiplexed streams can transmit the same frame bytes. The transport supplies target resolution and identity.

Type 1 (`OpenRequest`) and type 3 (`OpenRefused`) use header version zero so a peer can negotiate without knowing version one. Types 2 (`OpenResponse`), 4 (`Input`), 5 (`Resize`), 6 (`Control`), 7 (`ControlResult`), 8 (`Event`), 9 (`Snapshot`), and 10 (`Output`) use header version one. Unknown frame types and wrong header versions are errors. A frame with an unknown type under header version zero reports the version error first. Except for `Input`, `Snapshot`, and `Output`, payloads are UTF-8 JSON with the typed fields in `hypervisor_core::channel`. Unknown fields in structured payloads, missing required fields, malformed JSON, and zero terminal dimensions are rejected. The `resume` key is required in an open request. Its value may be `null`. Optional spawn `cwd` and `user` keys may be omitted. `Input` contains unchanged bytes, including NUL and invalid UTF-8. A frame body must fit the common size limit.

An open request contains an ordered `versions` array, target (`session` id or `spawn` spec), `mode`, `encoding`, client size, client capabilities, and optional resume token. The current server chooses version one if present, regardless of offered order. Otherwise it sends `OpenRefused` with `unsupported_version` and `[1]`. A spawn spec contains command, arguments, environment pairs, optional working directory and user, and session kind. The transport resolves or creates the target before calling the channel adapter. Run 17 supplies the common runtime driver. This version uses byte encoding. Grid encoding returns `unsupported_encoding`. A resume token beyond the current output offset returns `invalid_request`.

An accepted open attaches a viewer in the requested mode with its requested size and a 64 KiB output budget. It does not take the write lock. The response contains version one, the granted mode, the effective PTY size, the fixed server capability profile, and the next output byte offset as `starting_sequence`. For a fresh snapshot this is the snapshot's `next_sequence`; for replay it is the attachment offset, after the replayed bytes. Queued live output starts at or after that offset. The server profile identifies `agent-hypervisor` and sets flags for ANSI color, SGR mouse, bracketed paste, and synchronized output, derived from the run 10 fixed profile. Client capability flags are advertised, with no selection beyond encoding in this run. A read-write mode means eligible to take the lock, not ownership of it. The holder's size remains until that viewer takes the lock. The holder's event sequence is separate from the output byte sequence.

## Run 14 byte encoding and resume

`Snapshot` (type 9) has an eight-byte unsigned big-endian `next_sequence` followed by nonempty VT bytes. The server serializes the current emulator grid and its tracked modes on the actor thread. The VT begins with a terminal soft reset and clear. It redraws the active screen, restores tracked input modes, then places the cursor. An alternate screen snapshot enters that screen before drawing. The snapshot describes the state after all output bytes before `next_sequence`. Later `Output` frames begin there. A fresh attach sends this snapshot after `OpenResponse`.

`Output` (type 10) has an eight-byte unsigned big-endian `sequence` followed by nonempty raw PTY bytes. The offset identifies the first byte in its payload, including NUL or invalid UTF-8, and the next offset is `sequence + payload length`. The receiver must apply these frames in order and request a new snapshot if a gap appears. Both frame types reject a payload shorter than eight bytes, and an output offset must not overflow `u64` after adding its payload length. Replay is split at the common frame limit.

The resume token contains `next_sequence`, the first byte the client has not applied. If all bytes from that offset through the attachment point remain in the ring, the channel sends them as `Output` frames before queued live output. A token equal to the current offset doesn't replay bytes. If those bytes were evicted, the channel sends a fresh `Snapshot` instead. The actor's grid supplies that snapshot, and taking it discards the viewer's earlier queued output at the same actor turn, so subsequent live output starts at the snapshot offset. A token ahead of the attachment offset is refused. A stopped or unavailable holder is refused rather than sending an empty snapshot.

After viewer output overflows its 64 KiB queue, the existing `resync_required` event reports the oldest retained offset and the attachment is paused. The channel's `snapshot()` operation returns a fresh `Snapshot` and resumes live output. The transport added in run 16 must call that operation after sending the resync notice. A snapshot that exceeds the one MiB frame limit is refused on open or ends the channel on later resync.

## Run 13 input, control, and events

`Input` is sent to the holder only after this channel enters read-write mode. Input on a read-only channel is dropped. The holder then checks the writer and filters terminal replies before writing accepted bytes to the PTY. Input from a read-write channel without the lock is silently dropped after the holder refuses it. The client observes lock ownership through `writer_changed` events. `Resize` records the viewer's requested columns and rows. Only the current writer's size is applied, using the holder's last-size-wins settle point. A same-size request produces the holder's redraw hint. A zero dimension is malformed.

`Control` has `take`, `release`, `signal` (`hangup`, `interrupt`, `terminate`, or `kill`), and `detach`. Each produces `ControlResult` `accepted` or `refused`. Taking a read-only viewer promotes it to read-write and transfers the holder's lock, matching the merged viewer registry. Run 24 still must authorize who may take. Signals target the PTY foreground process group and require the lock. Detach removes the viewer and releases its lock. Dropping a channel detaches it.

An `Event` reports `mode_changed`, `size_changed`, `writer_changed`, `session_state_changed`, `session_exited`, or `resync_required`. Writer identity distinguishes viewer and programmatic sources. Mode and resync events go only to their viewer. Size, writer, state, and exit events go to every subscribed channel. State values are `unknown`, `idle`, `working`, `blocked_approval`, `blocked_input`, `blocked_unknown`, and `exited`. Other values are invalid. Exit includes code and signal. Resync reports the oldest retained output byte offset. The viewer remains attached but paused until a later snapshot, as the merged holder already does. The actor emits resync as a channel event through the run 13 adapter. Grid frames are run 15 work.

## Source and merged-contract differences

The source is `tbhb-dev/agent-orchestration-poc.internal` commit `f06e8f771f7288a98555a59646504ef32743326e`, `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/source.md`, lines 123 to 153 and 339. Run 13 follows the merged run 9 to 12 holder where the source and code differ:

| Source target | Run 13 contract |
| --- | --- |
| A slow viewer is dropped and told to resync. | The merged registry retains the attachment. It clears queued output and pauses live delivery before signaling resync. The channel relays that notice and follows the registry. |
| Input on a read-only channel is dropped. | The merged holder refuses such input. The channel drops it before calling the holder, so wire behavior follows the source and holder behavior remains intact. |
| Open can target a spawn spec. | Runs 9 to 12 expose `spawn` separately and have no target registry or common runtime driver. The wire contains a spawn spec, while the caller resolves it to a `SessionHandle` before channel open. A shared factory is run 17 work. |
| Open includes an encoding and resume token. | The merged actor has a byte ring and grid snapshot but no channel replay or grid encoding. Run 13 refuses grid and nonempty resume requests. Runs 14 and 15 own those behaviors. |
| The open response gives an effective size and starting sequence. | The merged holder applies size after lock ownership and settles resize batches. Its ring sequence counts bytes. Run 13 reads both in the actor turn that attaches the viewer and reports those values. |
| Read-only viewers cannot input, and control can take the lock. | The merged registry promotes a read-only viewer when it takes the lock. This run sends a mode event on that transition. The separate authorization gate remains run 24 work. |

Run 14 checked the same source at `tbhb-dev/agent-orchestration-poc.internal` commit `ca626726f064579a01360b883bf22016e34666ba`, `wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/source.md`, lines 132 to 136, and the run table's finding that a fresh snapshot comes from the server grid. The following differences remain:

| Source target | Run 14 contract |
| --- | --- |
| Resume from the client's last sequence number. | The merged ring counts byte offsets and `read_from` starts inclusively. The token contains the next byte the client needs. Using the last applied byte would duplicate it. |
| A slow viewer is dropped and told to resync. | The merged viewer registry retains a paused attachment. The channel preserves that behavior and exposes a fresh snapshot operation after its resync event. |
| Snapshot the screen and mode state as VT for xterm.js and native terminals. | The merged grid serializer restores the tracked modes but does not capture scroll regions, tab stops, charsets, hyperlinks, titles, palette changes, kitty keyboard flags, or pending wrap. Client rendering against xterm.js and native terminals remains untested until the later client phases. |

## Untested limits

- Client rendering of byte snapshots in xterm.js and native terminals remains unverified ([#44](https://github.com/tbhb-dev/agent-hypervisor/issues/44)). The Phase 5 snapshot-fidelity suite checks the serializer's omitted state against live harness screens.
- Grid frames, row diffs, frame-rate limits, and scrollback await run 15 ([#51](https://github.com/tbhb-dev/agent-hypervisor/issues/51)).
- Spawn targets require caller-side resolution until the runtime driver is connected in run 17 ([#52](https://github.com/tbhb-dev/agent-hypervisor/issues/52)).
- Frame behavior on Unix sockets, WebSockets, and multiplexed streams is untested until transports exist ([#53](https://github.com/tbhb-dev/agent-hypervisor/issues/53)).
- Client capability flags are advertised but only encoding is selected. Other negotiation behavior is undefined ([#54](https://github.com/tbhb-dev/agent-hypervisor/issues/54)).
- The merged holder promotes a read-only viewer on `take`. Caller authorization is untested and required before exposed clients use it ([#45](https://github.com/tbhb-dev/agent-hypervisor/issues/45)).

`crates/hypervisor-core/src/channel.rs` tests each frame type, malformed frames, truncated frames, version refusal, and round trips for arbitrary input and sequenced output bytes. The holder emits the mode, writer, and resync events named by the protocol. `crates/hypervisor-session/src/channel.rs` adapts decoded frames to the resolved session, with Unix PTY conformance cases in `crates/hypervisord/tests/conformance.rs` for fresh VT snapshots, live output, retained replay, evicted replay, future tokens, and overflow resync.

## Inputs

Findings below are in the private vault repository `tbhb-dev/agent-orchestration-poc.internal` at commit `69dc14c`.

- Serialize to viewers from grid reads, not the VT formatter, which restyles blank gaps: [RFC-36 run 2, lines 19 and 67](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r2-emulator.md#L19) and [RFC-36 run 3, line 22](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r3-harness-capture.md#L22).
- Specify mode-aware wheel and page-key routing, and decide which encoding carries scrollback: [RFC-36 run 4, lines 17 and 61](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T1944Z-RFC-36-agent-hypervisor-attach/findings/r4-prior-art.md#L17).
- Attribution comes from the listener, with one socket per workspace or session and peer credentials kept as audit fields: [RFC-40 run 12, lines 16 and 36](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-40-miscellany-spikes/findings/r12-ipc-sandbox-tiers.md#L16).
- Socket paths stay under the 104-byte `sun_path` limit, outside the workspace tree: [RFC-39 run 3, line 13](https://github.com/tbhb-dev/agent-orchestration-poc.internal/blob/69dc14c7a0e864329c4f8a4301e8688b67363fd4/wiki/proposals/2026-10-07T2052Z-RFC-39-filesystem-spikes/findings/r3-managed-directories.md#L13).
