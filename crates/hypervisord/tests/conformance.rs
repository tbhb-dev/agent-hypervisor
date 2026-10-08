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
use hypervisor_core::emulator::Size;
use hypervisor_core::session::{
    HolderConfig, Persistence, Refusal, RingRead, SessionEvent, SessionKind, SpawnSpec, ViewerId,
    ViewerMode, Writer,
};
use hypervisor_core::state::{AgentState, BlockReason, Harness, HookKind};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::{PtyError, Spawn, UnixPty, UnixSpawner};
use hypervisor_session::channel::Channel;
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
        let size = Size::new(80, 24).unwrap();
        let mut spec = SpawnSpec::new("/bin/sh", size, SessionKind::Shell);
        spec.args = vec!["-c".into(), script.into()];
        spec.env = vec![("PATH".into(), "/bin:/usr/bin".into())];
        let mut config = HolderConfig::new(Persistence::Persistent);
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
        assert!(!output(session).contains("got:blocked"));
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
        wait_for_output(session, "got:one");
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
        assert!(!output(session).contains("got:blocked2"));
        assert_eq!(
            channel.receive(Frame::Control(Control::Take)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
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
fn terminal_channel_refuses_unsupported_versions_and_unimplemented_encodings() {
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
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::UnsupportedEncoding))
    );
    request.encoding = Encoding::Bytes;
    request.resume = Some(hypervisor_core::channel::ResumeToken { next_sequence: 0 });
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::InvalidRequest))
    );
    fixture.finish(pgid);
}

#[test]
fn terminal_channel_reports_resync_after_viewer_overflow() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line; i=0; while [ $i -lt 70 ]; do printf '%1024s' x; i=$((i+1)); done; IFS= read -r line",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    {
        let (mut channel, _) = Channel::open(session, ViewerId(83), &open_request())
            .unwrap_or_else(|_| panic!("open refused"));
        channel.receive(Frame::Control(Control::Take)).unwrap();
        channel.receive(Frame::Input(b"go\n".to_vec())).unwrap();
        assert!(matches!(
            channel_event(&channel, |e| matches!(e, Event::ResyncRequired { .. })),
            Event::ResyncRequired { .. }
        ));
    }
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
