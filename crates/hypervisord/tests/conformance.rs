//! First server-side conformance cases through the public session handle.
//!
//! The Unix PTY is the only Phase 1 driver. Every fixture joins its actor after
//! closing, which signals the child's process group even on assertion failure.

use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{
    Capabilities, Control, ControlResult, Encoding, Event, Frame, FrameError, Mode, OpenRefusal,
    OpenRequest, OpenTarget, WireSignal, WireSize,
};
use hypervisor_core::channel_policy::ClientError;
use hypervisor_core::emulator::{Cursor, Modes, Size};
use hypervisor_core::grid_channel::GridFrame;
use hypervisor_core::session::{
    HolderConfig, Persistence, Refusal, RingRead, SessionEvent, SessionKind, SpawnSpec, ViewerId,
    ViewerMode, Writer,
};
use hypervisor_core::state::{AgentState, BlockReason, Harness, HookKind};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::{PtyError, Spawn, UnixPty, UnixSpawner};
use hypervisor_session::channel::{Channel, ChannelError};
use hypervisor_session::{Command, LogContext, SessionHandle, spawn};
use hypervisord::terminal_socket::TerminalSocket;

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

struct ServerRun {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<std::io::Result<()>>>,
}

struct SocketRoot(std::path::PathBuf);

impl SocketRoot {
    fn new(tag: &str) -> Self {
        Self(std::path::PathBuf::from(format!(
            "/tmp/hv16{tag}-{}",
            std::process::id()
        )))
    }
}

impl Drop for SocketRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl ServerRun {
    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

impl Drop for ServerRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_fixture(
    fixture: &mut Fixture,
    root: &Path,
    allowed_uid: u32,
) -> (ServerRun, std::path::PathBuf) {
    let socket =
        TerminalSocket::bind(root, "workspace", "conformance-session", allowed_uid).unwrap();
    let path = socket.path().to_owned();
    let session = fixture.session.take().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let result = socket.serve_until(&session, &running);
        session.send(Command::Close);
        session.join();
        result
    });
    (
        ServerRun {
            stop,
            thread: Some(thread),
        },
        path,
    )
}

fn wire_frame(stream: &mut UnixStream) -> Frame {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).unwrap();
    let length = u32::from_be_bytes(prefix) as usize;
    assert!((3..=hypervisor_core::channel::MAX_FRAME).contains(&length));
    let mut bytes = Vec::with_capacity(4 + length);
    bytes.extend_from_slice(&prefix);
    bytes.resize(4 + length, 0);
    stream.read_exact(&mut bytes[4..]).unwrap();
    Frame::decode(&bytes).unwrap().unwrap().0
}

fn wire_send(stream: &mut UnixStream, frame: &Frame) {
    stream.write_all(&frame.encode().unwrap()).unwrap();
}

#[test]
fn terminal_socket_accepts_local_peer_and_serves_byte_and_grid_viewers() {
    let mut fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let pgid = fixture_pgid(&wait_for_output(fixture.session(), "ready"));
    let root = SocketRoot::new("a");
    let uid = rustix::process::geteuid().as_raw();
    let (server, path) = serve_fixture(&mut fixture, &root.0, uid);
    let mut wrong = UnixStream::connect(&path).unwrap();
    wrong.set_read_timeout(Some(WAIT)).unwrap();
    let mut request = open_request();
    request.target = OpenTarget::Session("another-session".into());
    wire_send(&mut wrong, &Frame::OpenRequest(request));
    assert!(
        matches!(wire_frame(&mut wrong), Frame::OpenRefused(refused) if refused.reason == OpenRefusal::UnknownTarget)
    );
    let mut version = UnixStream::connect(&path).unwrap();
    version.set_read_timeout(Some(WAIT)).unwrap();
    let mut request = open_request();
    request.versions = vec![2];
    wire_send(&mut version, &Frame::OpenRequest(request));
    assert!(
        matches!(wire_frame(&mut version), Frame::OpenRefused(refused) if refused.reason == OpenRefusal::UnsupportedVersion)
    );
    let mut byte = UnixStream::connect(&path).unwrap();
    byte.set_read_timeout(Some(WAIT)).unwrap();
    wire_send(&mut byte, &Frame::OpenRequest(open_request()));
    assert!(matches!(wire_frame(&mut byte), Frame::OpenResponse(_)));
    assert!(matches!(wire_frame(&mut byte), Frame::Snapshot { .. }));
    let mut grid = UnixStream::connect(&path).unwrap();
    grid.set_read_timeout(Some(WAIT)).unwrap();
    let mut request = open_request();
    request.encoding = Encoding::Grid;
    wire_send(&mut grid, &Frame::OpenRequest(request));
    assert!(matches!(wire_frame(&mut grid), Frame::OpenResponse(_)));
    assert!(matches!(
        wire_frame(&mut grid),
        Frame::Grid(GridFrame::Full { frame: 1, .. })
    ));
    wire_send(&mut byte, &Frame::Input(b"blocked\n".to_vec()));
    wire_send(&mut byte, &Frame::Control(Control::Take));
    wire_send(&mut byte, &Frame::Input(b"wire\n".to_vec()));
    let mut saw_control = false;
    let mut saw_output = false;
    let mut output = Vec::new();
    for _ in 0..20 {
        match wire_frame(&mut byte) {
            Frame::ControlResult(ControlResult::Accepted) => saw_control = true,
            Frame::Output { bytes, .. } => {
                output.extend(bytes);
                if String::from_utf8_lossy(&output).contains("got:wire") {
                    saw_output = true;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(saw_control && saw_output);
    assert!(!String::from_utf8_lossy(&output).contains("got:blocked"));
    wire_send(&mut byte, &Frame::Control(Control::Detach));
    for _ in 0..20 {
        if wire_frame(&mut byte) == Frame::ControlResult(ControlResult::Accepted) {
            break;
        }
    }
    server.finish();
    fixture.finish(pgid);
}

#[test]
fn terminal_socket_refuses_peer_with_unlisted_uid() {
    let mut fixture = Fixture::start("printf 'pid:%s\\nready\\n' \"$$\"; IFS= read -r line");
    let pgid = fixture_pgid(&wait_for_output(fixture.session(), "ready"));
    let root = SocketRoot::new("r");
    let uid = rustix::process::geteuid().as_raw();
    let (server, path) = serve_fixture(&mut fixture, &root.0, uid.wrapping_add(1));
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
    server.finish();
    fixture.finish(pgid);
}

#[test]
fn terminal_socket_replaces_byte_snapshot_after_resync() {
    let mut fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line; i=0; while [ $i -lt 70 ]; do printf '%1024s' x; i=$((i+1)); done; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let pgid = fixture_pgid(&wait_for_output(fixture.session(), "ready"));
    let root = SocketRoot::new("s");
    let uid = rustix::process::geteuid().as_raw();
    let (server, path) = serve_fixture(&mut fixture, &root.0, uid);
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    wire_send(&mut stream, &Frame::OpenRequest(open_request()));
    assert!(matches!(wire_frame(&mut stream), Frame::OpenResponse(_)));
    assert!(matches!(wire_frame(&mut stream), Frame::Snapshot { .. }));
    wire_send(&mut stream, &Frame::Control(Control::Take));
    wire_send(&mut stream, &Frame::Input(b"go\n".to_vec()));
    let mut saw_resync = false;
    let mut next_sequence = None;
    for _ in 0..100 {
        match wire_frame(&mut stream) {
            Frame::Event(Event::ResyncRequired { .. }) => saw_resync = true,
            Frame::Snapshot {
                next_sequence: next,
                ..
            } if saw_resync => {
                next_sequence = Some(next);
                break;
            }
            _ => {}
        }
    }
    let next_sequence = next_sequence.expect("resync notice must be followed by a snapshot");
    wire_send(&mut stream, &Frame::Input(b"after-resync\n".to_vec()));
    let mut found = false;
    for _ in 0..100 {
        if let Frame::Output { sequence, bytes } = wire_frame(&mut stream) {
            assert_eq!(sequence, next_sequence);
            if String::from_utf8_lossy(&bytes).contains("got:after-resync") {
                found = true;
                break;
            }
        }
    }
    assert!(found, "live output must continue at the snapshot sequence");
    server.finish();
    fixture.finish(pgid);
}

#[test]
fn terminal_debug_prints_frames_and_replays_raw_input() {
    let mut fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let pgid = fixture_pgid(&wait_for_output(fixture.session(), "ready"));
    let root = SocketRoot::new("d");
    let input = root.0.join("input.raw");
    std::fs::create_dir_all(&root.0).unwrap();
    std::fs::write(&input, b"debug-client\n").unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (server, path) = serve_fixture(&mut fixture, &root.0, uid);
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_terminal-debug"))
        .arg(&path)
        .arg("conformance-session")
        .arg(&input)
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child_group = process_group::ProcessGroup::new(i32::try_from(child.id()).unwrap());
    let mut observer = UnixStream::connect(&path).unwrap();
    observer.set_read_timeout(Some(WAIT)).unwrap();
    wire_send(&mut observer, &Frame::OpenRequest(open_request()));
    assert!(matches!(wire_frame(&mut observer), Frame::OpenResponse(_)));
    assert!(matches!(wire_frame(&mut observer), Frame::Snapshot { .. }));
    let mut saw_replay = false;
    for _ in 0..30 {
        if let Frame::Output { bytes, .. } = wire_frame(&mut observer)
            && String::from_utf8_lossy(&bytes).contains("got:debug-client")
        {
            saw_replay = true;
            break;
        }
    }
    assert!(saw_replay, "debug client must replay its raw file");
    thread::sleep(Duration::from_millis(30));
    server.finish();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    let printed = String::from_utf8(result.stdout).unwrap();
    assert!(printed.contains("OpenResponse"));
    assert!(printed.contains("got:debug-client"));
    let _ = child_group.kill();
    fixture.finish(pgid);
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
    request.resume = Some(hypervisor_core::channel::ResumeToken { next_sequence: 0 });
    assert!(
        Channel::open(session, ViewerId(90), &request).err().is_some_and(|frame| matches!(*frame, Frame::OpenRefused(refused) if refused.reason == OpenRefusal::InvalidRequest))
    );
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
fn terminal_grid_full_diff_cap_and_resync() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let mut request = open_request();
    request.encoding = Encoding::Grid;
    request.max_frames_per_second = Some(2);
    {
        let (mut bytes, _) = Channel::open(session, ViewerId(93), &open_request()).unwrap();
        assert_eq!(
            bytes.poll_grid(Duration::ZERO),
            Err(hypervisor_session::channel::ChannelError::Client(
                hypervisor_core::channel_policy::ClientError::UnexpectedFrame
            ))
        );
        drop(bytes);
        let (mut channel, response) = Channel::open(session, ViewerId(91), &request)
            .unwrap_or_else(|_| panic!("grid open refused"));
        assert!(matches!(response, Frame::OpenResponse(_)));
        let Frame::Grid(GridFrame::Full {
            frame: 1, cells, ..
        }) = channel.poll_grid(Duration::ZERO).unwrap().unwrap()
        else {
            panic!("first grid frame is not full")
        };
        assert!(cells.iter().any(|cell| cell.cluster == "r"));
        assert_eq!(channel.poll_grid(Duration::from_millis(499)), Ok(None));
        channel.receive(Frame::Control(Control::Take)).unwrap();
        channel.receive(Frame::Input(b"one\n".to_vec())).unwrap();
        wait_for_output(session, "got:one");
        assert!(
            matches!(channel.poll_grid(Duration::from_millis(500)).unwrap(),
            Some(Frame::Grid(GridFrame::Diff { frame: 2, base_frame: 1, rows, .. })) if !rows.is_empty())
        );
        assert_eq!(
            channel.poll_grid(Duration::ZERO),
            Err(hypervisor_session::channel::ChannelError::Grid(
                FrameError::InvalidValue
            ))
        );
        assert_eq!(channel.poll_grid(Duration::from_millis(999)), Ok(None));
        channel.receive(Frame::Input(b"two\n".to_vec())).unwrap();
        wait_for_output(session, "got:two");
        assert!(matches!(
            channel.poll_grid(Duration::from_millis(1000)).unwrap(),
            Some(Frame::Grid(GridFrame::Diff {
                frame: 3,
                base_frame: 2,
                ..
            }))
        ));
        assert_eq!(channel.poll_grid(Duration::from_millis(1000)), Ok(None));
        assert_eq!(
            channel.receive(Frame::Grid(GridFrame::Diff {
                frame: 2,
                base_frame: 1,
                output_sequence: 0,
                size: WireSize { cols: 1, rows: 1 },
                rows: vec![],
                cursor: Cursor::default(),
                modes: Modes::default(),
                hyperlinks: vec![],
            })),
            Err(hypervisor_session::channel::ChannelError::Client(
                hypervisor_core::channel_policy::ClientError::UnexpectedFrame
            ))
        );
        assert_eq!(
            channel.receive(Frame::Control(Control::Detach)),
            Ok(Some(Frame::ControlResult(ControlResult::Accepted)))
        );
        assert_eq!(
            channel.poll_grid(Duration::from_millis(1500)),
            Err(hypervisor_session::channel::ChannelError::Client(
                hypervisor_core::channel_policy::ClientError::Detached
            ))
        );
    }
    fixture.finish(pgid);
}

#[test]
fn terminal_grid_overflow_forces_full_frame() {
    let fixture = Fixture::start(
        "printf 'pid:%s\\n' \"$$\"; stty -echo; printf 'ready\\n'; IFS= read -r line; i=0; while [ $i -lt 70 ]; do printf '%1024s' x; i=$((i+1)); done; IFS= read -r line",
    );
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let mut request = open_request();
    request.encoding = Encoding::Grid;
    {
        let (mut channel, _) = Channel::open(session, ViewerId(92), &request)
            .unwrap_or_else(|_| panic!("grid open refused"));
        assert!(matches!(
            channel.poll_grid(Duration::ZERO).unwrap(),
            Some(Frame::Grid(GridFrame::Full { frame: 1, .. }))
        ));
        channel.receive(Frame::Control(Control::Take)).unwrap();
        channel.receive(Frame::Input(b"go\n".to_vec())).unwrap();
        channel_event(&channel, |event| {
            matches!(event, Event::ResyncRequired { .. })
        });
        assert!(matches!(
            channel.poll_grid(Duration::from_millis(1)).unwrap(),
            Some(Frame::Grid(GridFrame::Full { frame: 2, .. }))
        ));
    }
    fixture.finish(pgid);
}

#[test]
fn terminal_grid_poll_after_actor_end_is_refused() {
    let fixture =
        Fixture::start("printf 'pid:%s\\n' \"$$\"; printf 'ready\\n'; while :; do sleep 1; done");
    let session = fixture.session();
    let pgid = fixture_pgid(&wait_for_output(session, "ready"));
    let mut request = open_request();
    request.encoding = Encoding::Grid;
    {
        let (mut channel, _) = Channel::open(session, ViewerId(94), &request).unwrap();
        assert!(matches!(
            channel.poll_grid(Duration::ZERO),
            Ok(Some(Frame::Grid(_)))
        ));
        session.send(Command::Close);
        let start = Instant::now();
        loop {
            match channel.poll_grid(Duration::from_millis(1)) {
                Err(hypervisor_session::channel::ChannelError::Refused(Refusal::UnknownViewer)) => {
                    break;
                }
                Ok(None | Some(_)) => {}
                other => panic!("unexpected grid result after close: {other:?}"),
            }
            assert!(start.elapsed() < WAIT, "actor did not end");
            thread::sleep(Duration::from_millis(10));
        }
    }
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
    wait_for_output(session, "got:away\r\n");
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
    wait_for_output(session, "got:live\r\n");
    let Frame::Output { sequence, bytes } = replay.next_output().unwrap().unwrap() else {
        panic!("missing replay")
    };
    assert_eq!(sequence, resume_at);
    assert_eq!(response.starting_sequence, resume_at + bytes.len() as u64);
    assert!(String::from_utf8_lossy(&bytes).contains("got:away"));
    let mut next_sequence = response.starting_sequence;
    let mut live = Vec::new();
    while live.len() < b"got:live\r\n".len() {
        let Frame::Output { sequence, bytes } = replay.next_output().unwrap().unwrap() else {
            panic!("missing live output after replay")
        };
        assert_eq!(sequence, next_sequence);
        next_sequence += bytes.len() as u64;
        live.extend(bytes);
    }
    assert_eq!(live, b"got:live\r\n");
    assert_eq!(replay.next_output(), Ok(None));
    drop(replay);
    request.resume = Some(hypervisor_core::channel::ResumeToken { next_sequence });
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
