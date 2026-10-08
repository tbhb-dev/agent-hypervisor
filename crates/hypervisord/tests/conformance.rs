//! First server-side conformance cases through the public session handle.
//!
//! The Unix PTY is the only Phase 1 driver. Every fixture joins its actor after
//! closing, which signals the child's process group even on assertion failure.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{
    Capabilities, Control, ControlResult, Encoding, Event, Frame, Mode, OpenRefusal, OpenRequest,
    OpenTarget, WireSignal, WireSize,
};
use hypervisor_core::channel_policy::ClientError;
use hypervisor_core::emulator::Size;
use hypervisor_core::session::{
    HolderConfig, Persistence, Refusal, RingRead, SessionEvent, SessionKind, SpawnSpec, ViewerId,
    ViewerMode, Writer,
};
use hypervisor_core::state::{AgentState, BlockReason, Harness, HookKind};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::{PtyError, Spawn, UnixPty, UnixSpawner};
use hypervisor_session::channel::{Channel, ChannelError};
use hypervisor_session::{Command, LogContext, SessionHandle, spawn};

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

const WAIT: Duration = Duration::from_secs(5);

struct FixtureSpawner(Arc<Mutex<Option<process_group::ProcessGroup>>>);

impl Spawn for FixtureSpawner {
    type Pty = UnixPty;

    fn spawn(&self, spec: &SpawnSpec) -> Result<UnixPty, PtyError> {
        let pty = UnixSpawner.spawn(spec)?;
        *self.0.lock().unwrap() = Some(process_group::ProcessGroup::new(pty.pid()));
        Ok(pty)
    }
}

struct Fixture {
    session: Option<SessionHandle>,
    _group: process_group::ProcessGroup,
}

impl Fixture {
    fn start(script: &str) -> Self {
        Self::start_with_budget(script, HolderConfig::DEFAULT_RING_BUDGET)
    }

    fn start_with_budget(script: &str, ring_budget: NonZeroUsize) -> Self {
        let size = Size::new(80, 24).unwrap();
        let mut spec = SpawnSpec::new("/bin/sh", size, SessionKind::Shell);
        spec.args = vec!["-c".into(), script.into()];
        spec.env = vec![("PATH".into(), "/bin:/usr/bin".into())];
        let mut config = HolderConfig::new(Persistence::Persistent);
        config.ring_budget = ring_budget;
        config.retain_exited = Duration::ZERO;
        config.kill_grace = Duration::from_millis(100);
        let group = Arc::new(Mutex::new(None));
        let session = spawn(
            FixtureSpawner(Arc::clone(&group)),
            spec,
            config,
            LogContext {
                workload_id: "conformance-workload".into(),
                session_id: "conformance-session".into(),
            },
            GhosttyEmulator::new,
        );
        let guard = group.lock().unwrap().take();
        Self {
            session: Some(session.unwrap()),
            _group: guard.expect("the session spawned a fixture group"),
        }
    }

    fn session(&self) -> &SessionHandle {
        self.session.as_ref().unwrap()
    }

    #[allow(
        unsafe_code,
        reason = "kill with signal zero checks the fixture process group"
    )]
    fn finish(mut self, pgid: i32) {
        self.close();
        // setsid makes the shell the process-group leader. The actor has joined
        // and its waiter has reaped that leader before this check.
        // SAFETY: signal zero only checks whether this positive fixture group id exists.
        let result = unsafe { libc::kill(-pgid, 0) };
        assert_eq!(result, -1, "fixture process group {pgid} survived");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    fn close(&mut self) {
        if let Some(session) = self.session.take() {
            session.send(Command::Close);
            session.join();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.close();
    }
}

fn output(session: &SessionHandle) -> String {
    match session.read_from(0) {
        Some(RingRead::Bytes(bytes)) => String::from_utf8(bytes).unwrap(),
        other => panic!("unexpected ring read: {other:?}"),
    }
}

fn wait_for_output(session: &SessionHandle, text: &str) -> String {
    let start = Instant::now();
    loop {
        let bytes = output(session);
        if bytes.contains(text) {
            return bytes;
        }
        assert!(start.elapsed() < WAIT, "missing {text:?} in {bytes:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_screen(session: &SessionHandle, text: &str) {
    let start = Instant::now();
    while !session
        .snapshot()
        .is_some_and(|screen| String::from_utf8_lossy(&screen).contains(text))
    {
        assert!(start.elapsed() < WAIT, "missing {text:?} on screen");
        thread::sleep(Duration::from_millis(10));
    }
}

fn event_until(
    session: &SessionHandle,
    wanted: impl Fn(SessionEvent) -> bool,
) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = session.events().recv_timeout(WAIT).unwrap().event;
        events.push(event);
        if wanted(event) {
            return events;
        }
    }
}

fn fixture_pgid(output: &str) -> i32 {
    output
        .split_whitespace()
        .find_map(|word| word.strip_prefix("pid:"))
        .unwrap()
        .parse()
        .unwrap()
}

fn open_request() -> OpenRequest {
    OpenRequest {
        versions: vec![1],
        target: OpenTarget::Session("conformance-session".into()),
        mode: Mode::ReadOnly,
        encoding: Encoding::Bytes,
        size: WireSize { cols: 80, rows: 24 },
        client: Capabilities {
            terminal: "test".into(),
            flags: 0,
        },
        resume: None,
        max_frames_per_second: None,
    }
}

fn channel_event(channel: &Channel<'_>, wanted: impl Fn(&Event) -> bool) -> Event {
    let start = Instant::now();
    loop {
        if let Some(Frame::Event(event)) = channel.next_event()
            && wanted(&event)
        {
            return event;
        }
        assert!(start.elapsed() < WAIT, "channel event not received");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn terminal_channel_open_input_resize_controls_and_events() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    {
        let (mut channel, response) = Channel::open(session, ViewerId(81), &open_request())
            .unwrap_or_else(|_| panic!("open refused"));
        let Frame::OpenResponse(response) = response else {
            panic!("wrong open response")
        };
        assert_eq!(response.version, 1);
        assert_eq!(response.granted_mode, Mode::ReadOnly);
        assert_eq!(response.effective_size, WireSize { cols: 80, rows: 24 });
        assert!(response.starting_sequence >= 5);
        let (mut detachable, _) = Channel::open(session, ViewerId(82), &open_request())
            .unwrap_or_else(|_| panic!("second open refused"));
        assert_eq!(
            detachable.receive(Frame::Control(Control::Detach)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        channel
            .receive(Frame::Input(b"blocked\n".to_vec()))
            .unwrap();
        assert_eq!(
            channel.receive(Frame::Control(Control::Signal(WireSignal::Interrupt))),
            Ok(Some(Frame::ControlResult(ControlResult::Refused)))
        );
        assert_eq!(
            channel.receive(Frame::Control(Control::Take)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        assert_eq!(
            channel_event(&channel, |e| matches!(
                e,
                Event::ModeChanged(Mode::ReadWrite)
            )),
            Event::ModeChanged(Mode::ReadWrite)
        );
        assert!(matches!(
            channel_event(&channel, |e| matches!(e, Event::WriterChanged(_))),
            Event::WriterChanged(Some(_))
        ));
        channel.receive(Frame::Input(b"one\n".to_vec())).unwrap();
        let observed = wait_for_output(session, "got:one");
        assert!(!observed.contains("got:blocked"));
        channel
            .receive(Frame::Resize(WireSize {
                cols: 100,
                rows: 30,
            }))
            .unwrap();
        assert_eq!(
            channel_event(&channel, |e| matches!(e, Event::SizeChanged(_))),
            Event::SizeChanged(WireSize {
                cols: 100,
                rows: 30
            })
        );
        session.send(Command::Hook {
            harness: Harness::Claude,
            kind: HookKind::UserPromptSubmit,
            seq: 1,
        });
        assert_eq!(
            channel_event(&channel, |e| matches!(e, Event::SessionStateChanged(_))),
            Event::SessionStateChanged("working".into())
        );
        assert_eq!(
            channel.receive(Frame::Control(Control::Release)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        assert_eq!(
            channel_event(&channel, |e| matches!(e, Event::WriterChanged(None))),
            Event::WriterChanged(None)
        );
        channel
            .receive(Frame::Input(b"blocked2\n".to_vec()))
            .unwrap();
        assert_eq!(
            channel.receive(Frame::Control(Control::Take)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        channel
            .receive(Frame::Input(b"sentinel\n".to_vec()))
            .unwrap();
        let observed = wait_for_output(session, "got:sentinel");
        assert!(!observed.contains("got:blocked2"));
        assert_eq!(
            channel.receive(Frame::Control(Control::Signal(WireSignal::Kill))),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        assert!(matches!(
            channel_event(&channel, |e| matches!(e, Event::SessionExited { .. })),
            Event::SessionExited { .. }
        ));
    }
    fixture.finish(pgid);
}

#[test]

fn terminal_channel_detached_refuses_output_and_snapshot() {
    let fixture = Fixture::start("printf 'pid:%s\\nready\\n' \"$$\"; IFS= read -r line");
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let (mut channel, _) = Channel::open(session, ViewerId(89), &open_request()).unwrap();
    channel.receive(Frame::Control(Control::Detach)).unwrap();
    assert_eq!(
        channel.next_output(),
        Err(ChannelError::Client(ClientError::Detached))
    );
    assert_eq!(
        channel.snapshot(),
        Err(ChannelError::Client(ClientError::Detached))
    );
    drop(channel);
    fixture.finish(pgid);
}

#[test]
fn terminal_channel_refuses_unsupported_versions_and_invalid_grid_requests() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let mut request = open_request();
    request.versions = vec![2];
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::UnsupportedVersion && refused.supported_versions == [1]))
    );
    request.versions = vec![1];
    request.encoding = Encoding::Grid;
    request.max_frames_per_second = Some(0);
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::InvalidRequest))
    );
    request.max_frames_per_second = None;
    request.encoding = Encoding::Bytes;
    request.resume = Some(hypervisor_core::channel::ResumeToken {
        next_sequence: u64::MAX,
    });
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::InvalidRequest))
    );
    fixture.finish(pgid);
}

#[test]
fn terminal_channel_reports_resync_after_viewer_overflow() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line; i=0; while [ $i -lt 70 ]; do printf '%1024s' x; i=$((i+1)); done; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    {
        let (mut channel, response) = Channel::open(session, ViewerId(83), &open_request())
            .unwrap_or_else(|_| panic!("open refused"));
        let Frame::OpenResponse(response) = response else {
            panic!("wrong open response")
        };
        assert!(matches!(
            channel.next_output(),
            Ok(Some(Frame::Snapshot { .. }))
        ));
        channel.receive(Frame::Control(Control::Take)).unwrap();
        channel.receive(Frame::Input(b"go\n".to_vec())).unwrap();
        let Event::ResyncRequired { oldest } =
            channel_event(&channel, |e| matches!(e, Event::ResyncRequired { .. }))
        else {
            panic!("wrong resync event")
        };
        let retained_oldest = match session.read_from(0) {
            Some(RingRead::Bytes(_)) => 0,
            Some(RingRead::Gone { oldest }) => oldest,
            other => panic!("unexpected ring read: {other:?}"),
        };
        assert_eq!(oldest, retained_oldest);
        assert_eq!(channel.next_output(), Ok(None));
        let Frame::Snapshot { next_sequence, .. } = channel.snapshot().unwrap() else {
            panic!("missing resync snapshot")
        };
        assert_eq!(channel.next_output(), Ok(None));
        channel
            .receive(Frame::Input(b"after-resync\n".to_vec()))
            .unwrap();
        wait_for_output(session, "got:after-resync");
        let Frame::Output { sequence, bytes } = channel.next_output().unwrap().unwrap() else {
            panic!("missing post-resync output")
        };
        assert_eq!(sequence, next_sequence);
        assert!(String::from_utf8_lossy(&bytes).contains("got:after-resync"));
        assert!(matches!(
            session.read_from(response.starting_sequence),
            Some(RingRead::Bytes(bytes)) if !bytes.is_empty()
        ));
    }
    fixture.finish(pgid);
}

#[test]
fn terminal_channel_snapshots_modes_and_streams_sequenced_output() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf '\\033[?1049h\\033[?25lmode-ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "mode-ready"));
    {
        let (mut channel, response) =
            Channel::open(session, ViewerId(84), &open_request()).unwrap();
        let Frame::OpenResponse(response) = response else {
            panic!("wrong open response")
        };
        let Frame::Snapshot {
            next_sequence,
            bytes,
        } = channel.next_output().unwrap().unwrap()
        else {
            panic!("missing snapshot")
        };
        assert_eq!(next_sequence, response.starting_sequence);
        assert!(bytes.windows(8).any(|part| part == b"\x1b[?1049h"));
        assert!(bytes.windows(6).any(|part| part == b"\x1b[?25l"));
        assert!(String::from_utf8_lossy(&bytes).contains("mode-ready"));
        assert_eq!(channel.next_output(), Ok(None));
        channel.receive(Frame::Control(Control::Take)).unwrap();
        channel.receive(Frame::Input(b"live\n".to_vec())).unwrap();
        wait_for_output(session, "got:live");
        let Frame::Output { sequence, bytes } = channel.next_output().unwrap().unwrap() else {
            panic!("missing output")
        };
        assert_eq!(sequence, next_sequence);
        assert!(String::from_utf8_lossy(&bytes).contains("got:live"));
        let encoded = Frame::Output {
            sequence,
            bytes: bytes.clone(),
        }
        .encode()
        .unwrap();
        assert_eq!(
            Frame::decode(&encoded),
            Ok(Some((Frame::Output { sequence, bytes }, encoded.len())))
        );
        channel.receive(Frame::Input(b"queued\n".to_vec())).unwrap();
        wait_for_output(session, "got:queued");
        let Frame::Snapshot { next_sequence, .. } = channel.snapshot().unwrap() else {
            panic!("missing replacement snapshot")
        };
        assert_eq!(channel.next_output(), Ok(None));
        channel.receive(Frame::Input(b"after\n".to_vec())).unwrap();
        wait_for_output(session, "got:after");
        let Frame::Output { sequence, bytes } = channel.next_output().unwrap().unwrap() else {
            panic!("missing output after replacement snapshot")
        };
        assert_eq!(sequence, next_sequence);
        assert!(String::from_utf8_lossy(&bytes).contains("got:after"));
    }
    fixture.finish(pgid);
}

#[test]
fn terminal_channel_replays_retained_bytes_and_snapshots_evicted_bytes() {
    let fixture = Fixture::start_with_budget(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
        NonZeroUsize::new(64).unwrap(),
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    session.take(Writer::Program(9)).unwrap();
    let (mut first, _) = Channel::open(session, ViewerId(85), &open_request()).unwrap();
    let Frame::Snapshot {
        next_sequence: resume_at,
        ..
    } = first.next_output().unwrap().unwrap()
    else {
        panic!("missing snapshot")
    };
    drop(first);
    session
        .submit(Writer::Program(9), b"away\n".to_vec())
        .unwrap();
    wait_for_output(session, "got:away");
    let mut request = open_request();
    request.resume = Some(hypervisor_core::channel::ResumeToken {
        next_sequence: resume_at,
    });
    let (mut replay, response) = Channel::open(session, ViewerId(86), &request).unwrap();
    let Frame::OpenResponse(response) = response else {
        panic!("wrong open response")
    };
    session
        .submit(Writer::Program(9), b"live\n".to_vec())
        .unwrap();
    wait_for_output(session, "got:live");
    let Frame::Output { sequence, bytes } = replay.next_output().unwrap().unwrap() else {
        panic!("missing replay")
    };
    assert_eq!(sequence, resume_at);
    assert_eq!(response.starting_sequence, resume_at + bytes.len() as u64);
    assert!(String::from_utf8_lossy(&bytes).contains("got:away"));
    let Frame::Output {
        sequence,
        bytes: live,
    } = replay.next_output().unwrap().unwrap()
    else {
        panic!("missing live output after replay")
    };
    assert_eq!(sequence, response.starting_sequence);
    assert!(String::from_utf8_lossy(&live).contains("got:live"));
    assert_eq!(replay.next_output(), Ok(None));
    drop(replay);
    request.resume = Some(hypervisor_core::channel::ResumeToken {
        next_sequence: sequence + live.len() as u64,
    });
    let (mut current, _) = Channel::open(session, ViewerId(88), &request).unwrap();
    assert_eq!(current.next_output(), Ok(None));
    drop(current);
    let mut long = vec![b'x'; 100];
    long.push(b'\n');
    session.submit(Writer::Program(9), long).unwrap();
    wait_for_screen(session, "xxxxxxxxxx");
    request.resume = Some(hypervisor_core::channel::ResumeToken { next_sequence: 0 });
    let (mut snapshot, response) = Channel::open(session, ViewerId(87), &request).unwrap();
    let Frame::OpenResponse(response) = response else {
        panic!("wrong open response")
    };
    let Frame::Snapshot {
        next_sequence,
        bytes,
    } = snapshot.next_output().unwrap().unwrap()
    else {
        panic!("missing replacement snapshot")
    };
    assert_eq!(response.starting_sequence, next_sequence);
    assert!(String::from_utf8_lossy(&bytes).contains("xxxxxxxxxx"));
    session
        .submit(Writer::Program(9), b"after-snapshot\n".to_vec())
        .unwrap();
    wait_for_screen(session, "got:after-snapshot");
    let Frame::Output { sequence, bytes } = snapshot.next_output().unwrap().unwrap() else {
        panic!("missing live output after replacement snapshot")
    };
    assert_eq!(sequence, next_sequence);
    assert!(String::from_utf8_lossy(&bytes).contains("got:after-snapshot"));
    drop(snapshot);
    fixture.finish(pgid);
}

#[test]
fn spawn_attach_detach_resume_and_exit() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; [ \"$line\" = quit ] && exit 7; done",
    );
    let session = fixture.session();
    let initial = wait_for_output(session, "ready");
    let pgid = fixture_pgid(&initial);
    let first = ViewerId(1);
    session
        .attach(
            first,
            ViewerMode::ReadOnly,
            Size::new(80, 24).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
    session.take(Writer::Program(9)).unwrap();
    session.detach(first).unwrap();
    let resume_at = initial.len() as u64;
    session
        .submit(Writer::Program(9), b"away\n".to_vec())
        .unwrap();
    wait_for_output(session, "got:away");
    assert!(
        matches!(session.read_from(resume_at), Some(RingRead::Bytes(bytes)) if String::from_utf8_lossy(&bytes).contains("got:away"))
    );
    let second = ViewerId(2);
    session
        .attach(
            second,
            ViewerMode::ReadOnly,
            Size::new(80, 24).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
    let (next, screen) = session.viewer_snapshot(second).unwrap();
    assert!(next >= resume_at);
    assert!(String::from_utf8_lossy(&screen).contains("got:away"));
    session
        .submit(Writer::Program(9), b"quit\n".to_vec())
        .unwrap();
    let events = event_until(session, |event| matches!(event, SessionEvent::Exited(_)));
    assert!(events.contains(&SessionEvent::Created));
    assert!(events.contains(&SessionEvent::Attached { viewer: first }));
    assert!(events.contains(&SessionEvent::Detached { viewer: first }));
    assert!(events.contains(&SessionEvent::Attached { viewer: second }));
    assert!(matches!(events.last(), Some(SessionEvent::Exited(exit)) if exit.code == Some(7)));
    fixture.finish(pgid);
}

#[test]
fn read_only_lock_handoff_and_state_events() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let readonly = ViewerId(1);
    let writer = ViewerId(2);
    let size = Size::new(80, 24).unwrap();
    for (id, mode) in [
        (readonly, ViewerMode::ReadOnly),
        (writer, ViewerMode::ReadWrite),
    ] {
        session
            .attach(id, mode, size, NonZeroUsize::new(4096).unwrap())
            .unwrap();
    }
    assert_eq!(
        session.submit(Writer::Viewer(readonly), b"bad\n".to_vec()),
        Err(Refusal::NotWriter)
    );
    session.take(Writer::Viewer(writer)).unwrap();
    session
        .submit(Writer::Viewer(writer), b"one\n".to_vec())
        .unwrap();
    wait_for_output(session, "got:one");
    session.take(Writer::Viewer(readonly)).unwrap();
    assert_eq!(
        session.submit(Writer::Viewer(writer), b"bad\n".to_vec()),
        Err(Refusal::NotWriter)
    );
    session
        .submit(Writer::Viewer(readonly), b"two\n".to_vec())
        .unwrap();
    wait_for_output(session, "got:two");
    session.send(Command::Hook {
        harness: Harness::Claude,
        kind: HookKind::UserPromptSubmit,
        seq: 1,
    });
    session.send(Command::Hook {
        harness: Harness::Claude,
        kind: HookKind::PermissionRequest,
        seq: 2,
    });
    let events = event_until(session, |event| {
        matches!(
            event,
            SessionEvent::StateChanged {
                to: AgentState::Blocked { .. },
                ..
            }
        )
    });
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::StateChanged {
            from: AgentState::Unknown,
            to: AgentState::Working
        }
    )));
    assert!(!output(session).contains("got:bad"));
    fixture.finish(pgid);
}

#[test]
fn unanswered_agy_tool_hook_blocks_after_the_prototype_deadline() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let start = Instant::now();
    session.send(Command::Hook {
        harness: Harness::Agy,
        kind: HookKind::PreToolUse,
        seq: 1,
    });
    let events = event_until(session, |event| {
        matches!(
            event,
            SessionEvent::StateChanged {
                to: AgentState::Blocked {
                    reason: BlockReason::Unknown
                },
                ..
            }
        )
    });
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::StateChanged {
            from: AgentState::Unknown,
            to: AgentState::Working
        }
    )));
    assert!(start.elapsed() >= Duration::from_millis(450));
    fixture.finish(pgid);
}
