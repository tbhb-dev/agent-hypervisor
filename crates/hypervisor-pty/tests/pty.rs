//! The Unix backend against `/bin/sh` children.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{SessionKind, Signal, SpawnError, SpawnSpec, Target};
use hypervisor_pty::{Pty, PtyError, Resize, Spawn, Support, UnixPty, UnixSpawner, Wait};

fn spec(script: &str, cols: u16, rows: u16) -> SpawnSpec {
    let mut s = SpawnSpec::new(
        "/bin/sh",
        Size::new(cols, rows).unwrap(),
        SessionKind::Shell,
    );
    s.args = vec!["-c".into(), script.into()];
    s.env = vec![("PATH".into(), "/bin:/usr/bin".into())];
    s
}

/// Collects a PTY's output on a thread.
struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    fn start(pty: &UnixPty) -> Self {
        let mut reader = pty.reader().unwrap();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = reader.read(&mut chunk) {
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        Self(buf)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    fn wait_for(&self, needle: &str) -> String {
        let start = Instant::now();
        loop {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no {needle:?} in {text:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn the_child_sees_the_spawn_size() {
    let mut pty = UnixSpawner
        .spawn(&spec("stty size; echo done", 100, 30))
        .unwrap();
    let out = Output::start(&pty);
    let text = out.wait_for("done");
    assert!(text.contains("30 100"), "{text:?}");
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (Some(0), None));
}

#[test]
fn the_exit_code_is_kept() {
    let mut pty = UnixSpawner.spawn(&spec("exit 7", 80, 24)).unwrap();
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (Some(7), None));
    assert!(pty.take_waiter().is_none());
}

#[test]
fn a_group_signal_reaches_background_jobs_and_the_signal_number_is_kept() {
    let mut pty = UnixSpawner
        .spawn(&spec("sleep 30 & echo ready; wait", 80, 24))
        .unwrap();
    let out = Output::start(&pty);
    out.wait_for("ready");
    assert_eq!(
        pty.signal(Signal::Terminate, Target::Group).unwrap(),
        Support::Ok
    );
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (None, Some(15)));
    // The background `sleep` shared the group, so the group empties once its orphan is reaped.
    let start = Instant::now();
    while pty.signal(Signal::Kill, Target::Group).is_ok() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the group outlived its leader"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_same_size_resize_sends_nothing_until_a_redraw_hint() {
    let script = "trap 'echo WINCH; stty size' WINCH; echo ready; while :; do sleep 0.02; done";
    let mut pty = UnixSpawner.spawn(&spec(script, 80, 24)).unwrap();
    let out = Output::start(&pty);
    out.wait_for("ready");
    assert_eq!(
        pty.resize(Size::new(80, 24).unwrap()).unwrap(),
        Resize::Unchanged
    );
    thread::sleep(Duration::from_millis(300));
    assert!(!out.text().contains("WINCH"));
    assert_eq!(pty.redraw_hint().unwrap(), Support::Ok);
    out.wait_for("WINCH");
    assert_eq!(
        pty.resize(Size::new(90, 20).unwrap()).unwrap(),
        Resize::Applied
    );
    out.wait_for("20 90");
    pty.signal(Signal::Kill, Target::Group).unwrap();
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!(exit.signal, Some(9));
}

#[test]
fn interrupt_reaches_the_foreground_group() {
    let script = "trap 'echo INT; exit 0' INT; echo ready; while :; do sleep 0.02; done";
    let mut pty = UnixSpawner.spawn(&spec(script, 80, 24)).unwrap();
    let out = Output::start(&pty);
    out.wait_for("ready");
    assert!(pty.foreground().is_some_and(|g| g > 0));
    pty.interrupt().unwrap();
    out.wait_for("INT");
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!(exit.code, Some(0));
}

#[test]
fn another_user_names_the_driver() {
    let mut s = spec("true", 80, 24);
    s.user = Some("nobody".into());
    let err = UnixSpawner.spawn(&s).unwrap_err();
    assert!(matches!(
        err,
        PtyError::Spec(SpawnError::OtherUser {
            driver: "container"
        })
    ));
}
