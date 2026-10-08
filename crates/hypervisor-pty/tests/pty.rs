//! The Unix backend against `/bin/sh` children.

use std::io::Read;
use std::ops::{Deref, DerefMut};
use std::os::unix::process::CommandExt;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{SessionKind, Signal, SpawnError, SpawnSpec, Target};
use hypervisor_pty::{Pty, PtyError, Resize, Spawn, Support, UnixPty, UnixSpawner, Wait};

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

struct Fixture {
    group: process_group::ProcessGroup,
    pty: UnixPty,
}

impl Deref for Fixture {
    type Target = UnixPty;

    fn deref(&self) -> &UnixPty {
        &self.pty
    }
}

impl DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut UnixPty {
        &mut self.pty
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.group.kill();
        if let Some(waiter) = self.pty.take_waiter() {
            let _ = waiter.wait();
        }
    }
}

fn spawn_fixture(script: &str, cols: u16, rows: u16) -> Fixture {
    let pty = UnixSpawner.spawn(&spec(script, cols, rows)).unwrap();
    Fixture {
        group: process_group::ProcessGroup::new(pty.pid()),
        pty,
    }
}

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

#[expect(
    unsafe_code,
    reason = "the test queries the supervisor's inherited signal disposition"
)]
fn with_ignored_signal(test: &str, name: &str, number: i32, child: impl FnOnce()) {
    if std::env::var_os("HYPERVISOR_TEST_IGNORED_SIGNAL").as_deref()
        == Some(std::ffi::OsStr::new(test))
    {
        // SAFETY: zero is a valid empty signal action for this query-only call.
        let mut inherited: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action queries the inherited disposition.
        let result = unsafe { libc::sigaction(number, std::ptr::null(), &raw mut inherited) };
        assert_eq!(result, 0);
        assert_eq!(inherited.sa_sigaction, libc::SIG_IGN);
        child();
        return;
    }

    let mut supervisor = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' \"$1\"; shift; exec \"$@\"",
            "sh",
            name,
            std::env::current_exe().unwrap().to_str().unwrap(),
            "--exact",
            test,
        ])
        .env("HYPERVISOR_TEST_IGNORED_SIGNAL", test)
        .process_group(0)
        .spawn()
        .unwrap();
    let _group = process_group::ProcessGroup::new(supervisor.id().cast_signed());
    assert!(supervisor.wait().unwrap().success());
}

fn assert_spawned_signal(signal: rustix::process::Signal, number: i32) {
    let mut pty = spawn_fixture("exec sleep 2", 80, 24);
    rustix::process::kill_process(rustix::process::Pid::from_raw(pty.pid()).unwrap(), signal)
        .unwrap();
    assert_eq!(
        pty.take_waiter().unwrap().wait().unwrap().signal,
        Some(number)
    );
}

#[test]
fn a_child_inherits_default_hangup_even_when_the_supervisor_ignores_it() {
    with_ignored_signal(
        "a_child_inherits_default_hangup_even_when_the_supervisor_ignores_it",
        "HUP",
        libc::SIGHUP,
        || {
            let mut pty = spawn_fixture("kill -HUP $$; echo survived", 80, 24);
            // Drain the master while the child exits; an ignored HUP writes output.
            let _out = Output::start(&pty);
            assert_eq!(pty.take_waiter().unwrap().wait().unwrap().signal, Some(1));
        },
    );
}

#[test]
fn a_child_inherits_default_interrupt_even_when_the_supervisor_ignores_it() {
    with_ignored_signal(
        "a_child_inherits_default_interrupt_even_when_the_supervisor_ignores_it",
        "INT",
        libc::SIGINT,
        || assert_spawned_signal(rustix::process::Signal::INT, libc::SIGINT),
    );
}

#[test]
fn a_child_inherits_default_quit_even_when_the_supervisor_ignores_it() {
    with_ignored_signal(
        "a_child_inherits_default_quit_even_when_the_supervisor_ignores_it",
        "QUIT",
        libc::SIGQUIT,
        || assert_spawned_signal(rustix::process::Signal::QUIT, libc::SIGQUIT),
    );
}

#[test]
fn a_child_inherits_default_terminate_even_when_the_supervisor_ignores_it() {
    with_ignored_signal(
        "a_child_inherits_default_terminate_even_when_the_supervisor_ignores_it",
        "TERM",
        libc::SIGTERM,
        || assert_spawned_signal(rustix::process::Signal::TERM, libc::SIGTERM),
    );
}

#[test]
fn a_child_inherits_default_pipe_even_when_the_supervisor_ignores_it() {
    with_ignored_signal(
        "a_child_inherits_default_pipe_even_when_the_supervisor_ignores_it",
        "PIPE",
        libc::SIGPIPE,
        || assert_spawned_signal(rustix::process::Signal::PIPE, libc::SIGPIPE),
    );
}

#[test]
fn ctrl_c_reaches_a_child_when_the_supervisor_ignores_interrupt() {
    with_ignored_signal(
        "ctrl_c_reaches_a_child_when_the_supervisor_ignores_interrupt",
        "INT",
        libc::SIGINT,
        || {
            let mut pty = spawn_fixture("stty -echo; echo ready; exec sleep 2", 80, 24);
            let out = Output::start(&pty);
            out.wait_for("ready");
            // The shell prints ready immediately before exec; let exec finish before Ctrl-C.
            thread::sleep(Duration::from_millis(50));
            pty.write(&[3]).unwrap();
            assert_eq!(
                pty.take_waiter().unwrap().wait().unwrap().signal,
                Some(libc::SIGINT)
            );
        },
    );
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
    let mut pty = spawn_fixture("stty size; echo done", 100, 30);
    let out = Output::start(&pty);
    let text = out.wait_for("done");
    assert!(text.contains("30 100"), "{text:?}");
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (Some(0), None));
}

#[test]
fn the_exit_code_is_kept() {
    let mut pty = spawn_fixture("exit 7", 80, 24);
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (Some(7), None));
    assert!(pty.take_waiter().is_none());
}

#[test]
fn a_group_signal_reaches_background_jobs_and_the_signal_number_is_kept() {
    let mut pty = spawn_fixture("sleep 30 & echo ready; wait", 80, 24);
    let out = Output::start(&pty);
    out.wait_for("ready");
    assert_eq!(
        pty.signal(Signal::Terminate, Target::Group).unwrap(),
        Support::Ok
    );
    let exit = pty.take_waiter().unwrap().wait().unwrap();
    assert_eq!((exit.code, exit.signal), (None, Some(15)));
    let group = rustix::process::Pid::from_raw(pty.pid()).unwrap();
    let start = Instant::now();
    while rustix::process::test_kill_process_group(group).is_ok() {
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
    let mut pty = spawn_fixture(script, 80, 24);
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
    let mut pty = spawn_fixture(script, 80, 24);
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

#[test]
fn a_panicking_fixture_kills_its_process_group() {
    let pid = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&pid);
    let panic = std::panic::catch_unwind(move || {
        let pty = spawn_fixture("echo ready; while :; do sleep 0.02; done", 80, 24);
        *seen.lock().unwrap() = Some(pty.pid());
        panic!("fixture panic");
    });
    assert!(panic.is_err());
    let pid = pid.lock().unwrap().unwrap();
    let group = rustix::process::Pid::from_raw(pid).unwrap();
    let start = Instant::now();
    while rustix::process::kill_process_group(group, rustix::process::Signal::KILL).is_ok() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the panicking fixture left its process group alive"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
