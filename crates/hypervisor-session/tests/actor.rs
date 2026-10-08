//! The session actor with ghostty-vt and `/bin/sh` children.

use std::num::NonZeroUsize;
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{
    Exit, HolderConfig, Persistence, RingRead, SessionEvent, SessionKind, SpawnSpec,
};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::UnixSpawner;
use hypervisor_session::{Command, SessionHandle, spawn};

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
    session.send(Command::Resize(size(80, 24)));
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
    for s in [size(100, 30), size(90, 20), size(132, 43), size(120, 40)] {
        session.send(Command::Resize(s));
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
    session.send(Command::Viewers(1));
    session.send(Command::Viewers(0));
    let events = events_until(&session, exited);
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
            session.send(Command::Resize(s));
            thread::sleep(Duration::from_millis(2));
        }
    }
    session.send(Command::Resize(size(100, 30)));
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
