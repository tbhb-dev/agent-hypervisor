use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::thread;

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{SessionKind, SpawnSpec};
use hypervisor_pty::{Pty, Spawn, UnixPty, UnixSpawner, Wait};

#[allow(dead_code)]
#[path = "../../../tests/support/process_group.rs"]
mod process_group;

struct Fixture {
    group: process_group::ProcessGroup,
    pty: UnixPty,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.group.kill();
        if let Some(waiter) = self.pty.take_waiter() {
            waiter.wait().unwrap();
        }
    }
}

fn spawn(path: &Path) -> Fixture {
    let mut spec = SpawnSpec::new("/bin/sh", Size::new(80, 24).unwrap(), SessionKind::Shell);
    spec.args = vec![
        "-c".into(),
        "trap '' HUP; while :; do sleep 0.02; done".into(),
    ];
    spec.env = vec![("PATH".into(), "/bin:/usr/bin".into())];
    let pty = UnixSpawner.spawn(&spec).unwrap();
    Fixture {
        group: process_group::ProcessGroup::new_in(pty.pid(), path),
        pty,
    }
}

fn check(path: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/check-fixture-groups.sh"))
        .arg(path)
        .output()
        .unwrap()
}

#[test]
fn concurrent_fixture_records_cannot_hide_a_leak_or_corruption() {
    let path =
        std::env::temp_dir().join(format!("hypervisor-fixture-groups-{}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let threads: Vec<_> = (0..24)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || spawn(&path))
        })
        .collect();
    let mut fixtures: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let records = fs::read_to_string(&path).unwrap();
    assert_eq!(records.lines().count(), 24);
    assert!(records.lines().all(|line| line.parse::<u32>().is_ok()));
    for fixture in fixtures.drain(1..) {
        drop(fixture);
    }
    let leaked = fixtures.pop().unwrap();
    let result = check(&path);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("still alive"));
    drop(leaked);
    fs::write(&path, "12x\n").unwrap();
    let result = check(&path);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("malformed"));
    fs::remove_file(path).unwrap();
}

#[test]
fn a_reaped_leader_is_not_signaled_by_its_guard() {
    let path = std::env::temp_dir().join(format!("hypervisor-reaped-group-{}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let mut spec = SpawnSpec::new("/bin/sh", Size::new(80, 24).unwrap(), SessionKind::Shell);
    spec.args = vec!["-c".into(), "exit 0".into()];
    let mut pty = UnixSpawner.spawn(&spec).unwrap();
    let mut group = process_group::ProcessGroup::new_in(pty.pid(), &path);
    assert_eq!(pty.take_waiter().unwrap().wait().unwrap().code, Some(0));
    assert!(!group.kill());
    assert!(!group.kill());
    fs::remove_file(path).unwrap();
}
