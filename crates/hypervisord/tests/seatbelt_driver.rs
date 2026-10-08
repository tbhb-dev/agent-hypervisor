//! Seatbelt workloads: the host shim lifecycle with every session under the generated profile.
#![cfg(target_os = "macos")]

use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{SessionKind, WireSize, WireSpawnSpec};
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, Recovery, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::driver::HostDriver;
use rustix::process::{self, Pid};

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

struct Fixture {
    root: PathBuf,
    id: StableId,
    groups: Vec<process_group::ProcessGroup>,
}

impl Fixture {
    fn new(name: &str) -> (Self, HostDriver) {
        let root = PathBuf::from(format!("/private/tmp/hv-r19-{}-{name}", std::process::id()));
        let (ws, cache) = (root.join("ws"), root.join("cache"));
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let id = StableId {
            host: "testhost".into(),
            local: "sandboxed".into(),
        };
        let mut driver = HostDriver::open(&root.join("rt"), &id.host, shim_exe()).unwrap();
        driver
            .create(WorkloadSpec {
                id: id.clone(),
                runtime: Runtime::Seatbelt,
                isolation: Isolation::Seatbelt,
                image: None,
                mounts: vec![],
                env: vec![],
                credential_refs: vec![],
                network: NetworkPolicy::Deny,
                resources: ResourceLimits {
                    memory_bytes: None,
                    cpu_count: None,
                },
                workspace_dir: ws,
                cache_dir: cache,
            })
            .unwrap();
        driver.start(&id).unwrap();
        let shim = driver.shim_pid(&id).unwrap().cast_signed();
        let fixture = Self {
            root,
            id,
            groups: vec![process_group::ProcessGroup::new(shim)],
        };
        (fixture, driver)
    }

    /// Run `script` in a session, wait for it to write `done`, and return the session PID.
    fn session(&mut self, driver: &HostDriver, script: &str) -> i32 {
        let ws = self.root.join("ws");
        driver
            .spawn_session(
                &self.id,
                StableId {
                    host: self.id.host.clone(),
                    local: "session".into(),
                },
                WireSpawnSpec {
                    command: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        format!("echo $$ > pid; {script}; echo > done; exec sleep 120"),
                    ],
                    env: vec![("PATH".into(), "/bin:/usr/bin".into())],
                    cwd: None,
                    user: None,
                    kind: SessionKind::Shell,
                },
                WireSize { cols: 80, rows: 24 },
            )
            .unwrap();
        let started = Instant::now();
        while !ws.join("done").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "session never finished"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let pid = fs::read_to_string(ws.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        self.groups.push(process_group::ProcessGroup::new(pid));
        pid
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join("ws").join(name)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut driver) = HostDriver::open(&self.root.join("rt"), &self.id.host, shim_exe()) {
            let _ = driver.stop(&self.id);
            let _ = driver.destroy(&self.id);
        }
        for group in &mut self.groups {
            group.kill();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn shim_exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_host-shim"))
}

#[test]
fn session_writes_only_inside_its_roots_and_cannot_reach_the_driver() {
    let (mut fixture, driver) = Fixture::new("access");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = fixture.root.display().to_string();
    let home = std::env::home_dir().unwrap().display().to_string();
    fixture.session(
        &driver,
        &format!(
            "echo in > inside; echo $? > s-inside; echo out > {root}/outside; echo $? > s-outside; \
             cat {root}/rt/*.json; echo $? > s-root; ls {home}; echo $? > s-home; \
             nc -z -G 1 127.0.0.1 {port}; echo $? > s-net"
        ),
    );
    assert_eq!(fixture.read("inside"), "in\n");
    assert_eq!(fixture.read("s-inside"), "0\n");
    for status in ["s-outside", "s-root", "s-home", "s-net"] {
        assert_ne!(fixture.read(status), "0\n", "{status} was allowed");
    }
    assert!(!fixture.root.join("outside").exists());
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn stop_ends_the_sandboxed_group_and_a_new_daemon_adopts_the_shim() {
    let (mut fixture, driver) = Fixture::new("lifecycle");
    let pid = fixture.session(&driver, "true");
    drop(driver);
    let mut recovered =
        HostDriver::open(&fixture.root.join("rt"), &fixture.id.host, shim_exe()).unwrap();
    assert_eq!(
        recovered.workloads()[&fixture.id.label()].recovery,
        Recovery::Running
    );
    assert_eq!(recovered.list_sessions(&fixture.id).unwrap().len(), 1);
    recovered.stop(&fixture.id).unwrap();
    assert!(
        process::test_kill_process_group(Pid::from_raw(pid).unwrap()).is_err(),
        "stop left the sandboxed process group alive"
    );
    assert_eq!(
        recovered.workloads()[&fixture.id.label()].recovery,
        Recovery::Stopped
    );
}
