//! The session actor with ghostty-vt and `/bin/sh` children.

use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{
    Exit, HolderConfig, Persistence, Refusal, RingRead, SessionEvent, SessionKind, SpawnSpec,
    ViewerId, ViewerMode, ViewerRead, Writer,
};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::UnixSpawner;
use hypervisor_session::{Command, SessionHandle, spawn};

const VIEWER: ViewerId = ViewerId(1);

fn attach_writer(session: &SessionHandle) {
    session
        .attach(
            VIEWER,
            ViewerMode::ReadWrite,
            size(80, 24),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
    session.take(Writer::Viewer(VIEWER)).unwrap();
}

const WAIT: Duration = Duration::from_secs(5);

fn size(cols: u16, rows: u16) -> Size {
    Size::new(cols, rows).unwrap()
}

fn start(script: &str, config: HolderConfig) -> SessionHandle {
    let mut spec = SpawnSpec::new("/bin/sh", size(80, 24), SessionKind::Shell);
    spec.args = vec!["-c".into(), script.into()];
    spec.env = vec![("PATH".into(), "/bin:/usr/bin".into())];
    spawn(UnixSpawner, spec, config, GhosttyEmulator::new).unwrap()
}

fn persistent() -> HolderConfig {
    HolderConfig::new(Persistence::Persistent)
}

fn output(session: &SessionHandle) -> String {
    match session.read_from(0) {
        Some(RingRead::Bytes(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        other => panic!("ring read from 0: {other:?}"),
    }
}

fn wait_for_output(session: &SessionHandle, needle: &str) -> String {
    let start = Instant::now();
    loop {
        let text = output(session);
        if text.contains(needle) {
            return text;
        }
        assert!(start.elapsed() < WAIT, "no {needle:?} in {text:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// The events up to and including the first that matches `stop`.
fn events_until(
    session: &SessionHandle,
    stop: impl Fn(&SessionEvent) -> bool,
) -> Vec<SessionEvent> {
    let mut seen = Vec::new();
    loop {
        let event = session
            .events()
            .recv_timeout(WAIT)
            .unwrap_or_else(|e| panic!("{e} after {seen:?}"));
        seen.push(event);
        if stop(&event) {
            return seen;
        }
    }
}

fn exited(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::Exited(_))
}

#[test]
fn a_da1_query_is_answered_with_no_viewer() {
    // With echo off, only the child's own line can contain "got:".
    let script = r#"stty raw -echo; printf '\033[c'; r=$(dd bs=1 count=9 2>/dev/null); printf 'got:%s\n' "$r" | cat -v"#;
    let session = start(script, persistent());
    let text = wait_for_output(&session, "got:");
    assert!(text.contains("got:^[[?62;22c"), "{text:?}");
}

#[test]
fn three_viewers_share_one_writer_and_programmatic_input_obeys_the_lock() {
    let session = start(
        "stty -echo; echo ready; while IFS= read -r line; do printf 'got:%s:' \"$line\"; stty size; done",
        persistent(),
    );
    wait_for_output(&session, "ready");
    for (id, mode, cols) in [
        (ViewerId(1), ViewerMode::ReadOnly, 60),
        (ViewerId(2), ViewerMode::ReadOnly, 100),
        (ViewerId(3), ViewerMode::ReadWrite, 120),
    ] {
        session
            .attach(id, mode, size(cols, 30), NonZeroUsize::new(4096).unwrap())
            .unwrap();
    }
    assert_eq!(
        session.submit(Writer::Viewer(ViewerId(1)), b"bad\n".to_vec()),
        Err(Refusal::NotWriter)
    );
    session.take(Writer::Viewer(ViewerId(3))).unwrap();
    events_until(&session, |e| *e == SessionEvent::Resized(size(120, 30)));
    assert_eq!(
        session.submit(Writer::Program(9), b"bad\n".to_vec()),
        Err(Refusal::NotWriter)
    );
    session
        .submit(Writer::Viewer(ViewerId(3)), b"one\n".to_vec())
        .unwrap();
    wait_for_output(&session, "got:one:30 120");
    session.take(Writer::Viewer(ViewerId(2))).unwrap();
    events_until(&session, |e| *e == SessionEvent::Resized(size(100, 30)));
    assert_eq!(
        session.submit(Writer::Viewer(ViewerId(3)), b"bad\n".to_vec()),
        Err(Refusal::NotWriter)
    );
    session
        .submit(Writer::Viewer(ViewerId(2)), b"two\n".to_vec())
        .unwrap();
    wait_for_output(&session, "got:two:30 100");
    session.take(Writer::Program(9)).unwrap();
    session
        .submit(Writer::Program(9), b"three\n".to_vec())
        .unwrap();
    wait_for_output(&session, "got:three:30 100");
    assert!(!output(&session).contains("got:bad:"));
}

#[test]
fn a_slow_viewer_gets_one_resync_then_a_fresh_snapshot() {
    let session = start(
        "stty -echo; echo ready; IFS= read -r line; printf '0123456789'; IFS= read -r line; printf zz",
        persistent(),
    );
    wait_for_output(&session, "ready");
    session
        .attach(
            VIEWER,
            ViewerMode::ReadOnly,
            size(80, 24),
            NonZeroUsize::new(4).unwrap(),
        )
        .unwrap();
    session.take(Writer::Viewer(VIEWER)).unwrap();
    session.viewer_snapshot(VIEWER).unwrap();
    session
        .submit(Writer::Viewer(VIEWER), b"go\n".to_vec())
        .unwrap();
    wait_for_output(&session, "0123456789");
    let notice = session.read_viewer(VIEWER).unwrap();
    assert!(matches!(notice, ViewerRead::Resync { .. }), "{notice:?}");
    assert_eq!(session.read_viewer(VIEWER).unwrap(), notice);
    let (next, snapshot) = session.viewer_snapshot(VIEWER).unwrap();
    assert!(next >= 10);
    assert!(String::from_utf8_lossy(&snapshot).contains("0123456789"));
    session
        .submit(Writer::Viewer(VIEWER), b"go\n".to_vec())
        .unwrap();
    wait_for_output(&session, "zz");
    assert!(matches!(
        session.read_viewer(VIEWER).unwrap(),
        ViewerRead::Output { .. }
    ));
}

#[test]
fn viewer_terminal_replies_never_reach_the_child() {
    let session = start(
        "stty -echo; echo ready; IFS= read -r line; printf 'got:%s\\n' \"$line\"",
        persistent(),
    );
    wait_for_output(&session, "ready");
    session
        .attach(
            VIEWER,
            ViewerMode::ReadOnly,
            size(80, 24),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
    assert_eq!(
        session.submit(Writer::Viewer(VIEWER), b"\x1b[?62;22c".to_vec()),
        Ok(())
    );
    session.take(Writer::Viewer(VIEWER)).unwrap();
    session
        .submit(Writer::Viewer(VIEWER), b"\x1b[3;5Rgood\n".to_vec())
        .unwrap();
    wait_for_output(&session, "got:good");
    assert!(!output(&session).contains("got:^[["));
}

#[test]
fn codex_batch_replies_are_ordered_with_no_viewer() {
    let script = r#"stty raw -echo; printf '\033[6n\033]10;?\033\\\033]11;?\033\\\033[?u\033[c'; r=$(dd bs=1 count=65 2>/dev/null); printf 'got:%s\n' "$r" | cat -v"#;
    let session = start(script, persistent());
    let text = wait_for_output(&session, "got:");
    assert!(
        text.contains(
            "got:^[[1;1R^[]10;rgb:d0d0/d0d0/d0d0^[\\^[]11;rgb:1c1c/1c1c/1c1c^[\\^[[?62;22c"
        ),
        "{text:?}"
    );
}

#[test]
fn osc_11_reply_precedes_a_later_cpr() {
    let script = r#"stty raw -echo; printf '\033]11;?\007\033[6n'; r=$(dd bs=1 count=30 2>/dev/null); printf 'got:%s\n' "$r" | cat -v"#;
    let session = start(script, persistent());
    let text = wait_for_output(&session, "got:");
    assert!(
        text.contains("got:^[]11;rgb:1c1c/1c1c/1c1c^G^[[1;1R"),
        "{text:?}"
    );
}

#[test]
fn a_pre_raw_da1_reply_is_echoed_by_the_line_discipline() {
    let session = start(
        r"printf '\033[c'; sleep 0.1; stty raw -echo; echo ready",
        persistent(),
    );
    let text = wait_for_output(&session, "ready");
    assert!(text.contains("^[[?62;22c"), "{text:?}");
}

#[test]
fn an_evicted_sequence_reads_as_gone() {
    let mut config = persistent();
    config.ring_budget = NonZeroUsize::new(64).unwrap();
    let session = start(
        "i=0; while [ $i -lt 50 ]; do echo line-$i; i=$((i+1)); done",
        config,
    );
    events_until(&session, exited);
    let Some(RingRead::Gone { oldest }) = session.read_from(0) else {
        panic!("sequence 0 was not evicted");
    };
    let Some(RingRead::Bytes(tail)) = session.read_from(oldest) else {
        panic!("the oldest retained sequence did not read");
    };
    assert_eq!(tail.len(), 64);
    assert!(String::from_utf8_lossy(&tail).ends_with("line-49\r\n"));
}

#[test]
fn a_same_size_resize_sends_a_redraw_hint_and_no_resize() {
    let script = "trap 'echo WINCH; stty size' WINCH; echo ready; while :; do sleep 0.02; done";
    let session = start(script, persistent());
    wait_for_output(&session, "ready");
    attach_writer(&session);
    session.viewer_resize(VIEWER, size(80, 24)).unwrap();
    wait_for_output(&session, "WINCH");
    wait_for_output(&session, "24 80");
    assert!(
        session
            .events()
            .try_iter()
            .all(|e| e == SessionEvent::Running),
        "a same-size request emitted a resize"
    );
}

#[test]
fn a_burst_of_resizes_ends_at_the_last_size() {
    let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.02; done";
    let session = start(script, persistent());
    wait_for_output(&session, "ready");
    attach_writer(&session);
    for s in [size(100, 30), size(90, 20), size(132, 43), size(120, 40)] {
        session.viewer_resize(VIEWER, s).unwrap();
    }
    let resized = events_until(&session, |e| *e == SessionEvent::Resized(size(120, 40)));
    let count = resized
        .iter()
        .filter(|e| matches!(e, SessionEvent::Resized(_)))
        .count();
    assert!((1..=4).contains(&count));
    wait_for_output(&session, "40 120");
    eprintln!("resize burst: 4 requests, {count} applied");
}

#[test]
fn the_final_screen_is_kept_until_the_grace_and_then_reaped() {
    let mut config = persistent();
    config.retain_exited = Duration::from_millis(300);
    let session = start("echo goodbye-screen; exit 4", config);
    let events = events_until(&session, exited);
    let at = Instant::now();
    let Some(SessionEvent::Exited(Exit { code, signal, .. })) = events.last().copied() else {
        unreachable!()
    };
    assert_eq!((code, signal), (Some(4), None));
    let screen = session.snapshot().expect("a screen during the grace");
    assert!(String::from_utf8_lossy(&screen).contains("goodbye-screen"));
    events_until(&session, |e| *e == SessionEvent::Reaped);
    assert!(at.elapsed() >= Duration::from_millis(250));
    assert_eq!(session.snapshot(), None);
    session.join();
}

#[test]
fn an_ephemeral_session_ends_when_its_viewers_leave() {
    let mut config = HolderConfig::ephemeral();
    config.retain_exited = Duration::ZERO;
    let session = start("echo ready; while :; do sleep 0.02; done", config);
    wait_for_output(&session, "ready");
    session
        .attach(
            VIEWER,
            ViewerMode::ReadOnly,
            size(80, 24),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
    session.detach(VIEWER).unwrap();
    let events = events_until(&session, exited);
    // #31 tracks the environment-dependent hangup signal failure; keep this assertion strict.
    assert_eq!(
        events.last(),
        Some(&SessionEvent::Exited(Exit {
            code: None,
            signal: Some(1),
            raw: Some(1)
        }))
    );
    events_until(&session, |e| *e == SessionEvent::Reaped);
    session.join();
}

/// herdr#4762: ghostty-vt crashed in resize after entering the alternate screen.
#[test]
fn repeated_resizes_on_the_alternate_screen_do_not_crash() {
    let script = r"printf '\033[?1049h\033[2J'; i=0; while :; do printf 'alt-%s\n' $i; i=$((i+1)); sleep 0.01; done";
    let session = start(script, persistent());
    wait_for_output(&session, "alt-3");
    attach_writer(&session);
    let sizes = [
        size(80, 24),
        size(1, 1),
        size(200, 60),
        size(40, 10),
        size(300, 100),
        size(2, 50),
    ];
    // A pause after each request keeps most of them out of one batch, so most are applied.
    for _ in 0..50 {
        for s in sizes {
            session.viewer_resize(VIEWER, s).unwrap();
            thread::sleep(Duration::from_millis(2));
        }
    }
    session.viewer_resize(VIEWER, size(100, 30)).unwrap();
    let events = events_until(&session, |e| *e == SessionEvent::Resized(size(100, 30)));
    let applied = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Resized(_)))
        .count();
    assert!(applied > 100, "only {applied} resizes were applied");
    eprintln!("alternate screen: 301 requests, {applied} applied");
    let before = output(&session).len();
    let start = Instant::now();
    while output(&session).len() == before {
        assert!(start.elapsed() < WAIT, "no output after the resizes");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(session.snapshot().is_some());
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A group member that ignores the hangup still holds the terminal after the leader exits.
#[test]
fn close_kills_a_group_member_that_ignores_the_hangup() {
    let mut config = persistent();
    config.kill_grace = Duration::from_millis(300);
    config.retain_exited = Duration::from_millis(1000);
    let session = start(
        r#"(trap '' HUP; exec sleep 30) & echo "pid=$!"; wait"#,
        config,
    );
    let text = wait_for_output(&session, "\r\n");
    let pid = text
        .split("pid=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the child printed its background PID")
        .to_owned();
    assert!(alive(&pid));
    session.send(Command::Close);
    let events = events_until(&session, exited);
    // #31 tracks the environment-dependent hangup signal failure; keep this assertion strict.
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Exited(Exit {
            signal: Some(1),
            ..
        }))
    ));
    let start = Instant::now();
    while alive(&pid) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{pid} outlived the kill grace"
        );
        thread::sleep(Duration::from_millis(20));
    }
    events_until(&session, |e| *e == SessionEvent::Reaped);
    session.join();
}
