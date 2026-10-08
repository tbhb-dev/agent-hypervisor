//! Host shims under launchd: daemon restart, daemon upgrade, and crash restart.
//!
//! Every job lives in the user's `gui/<uid>` domain under a `dev.tbhb.hypervisor.test.`
//! label, loads from a temporary plist, and is booted out by `JobGuard` on every exit path.
#![cfg(target_os = "macos")]

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{
    Capabilities, Encoding, Frame, Mode, OpenRequest, OpenTarget, SessionKind, WireSize,
    WireSpawnSpec,
};
use hypervisor_core::launchd::JobConfig;
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, Recovery, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::driver::HostDriver;
use rustix::process::{self, Pid, Signal};

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

use process_group::ProcessGroup;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Boots out its launchd job on drop, including when the test panics.
struct JobGuard(String);

impl JobGuard {
    fn new(service: String) -> Self {
        // The post-suite check boots out and fails on any recorded job that is still loaded.
        if let Some(path) = std::env::var_os("HYPERVISOR_TEST_LAUNCHD") {
            let mut file = OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(format!("{service}\n").as_bytes()).unwrap();
        }
        Self(service)
    }

    fn loaded(&self) -> bool {
        launchctl(&["print", &self.0]).status().unwrap().success()
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if self.loaded() {
            let _ = launchctl(&["bootout", &self.0]).status();
        }
    }
}

fn launchctl(args: &[&str]) -> Command {
    let mut command = Command::new("launchctl");
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

struct Fixture {
    root: PathBuf,
    id: StableId,
    config: JobConfig,
    shim_exe: PathBuf,
    job: Option<JobGuard>,
    groups: Vec<ProcessGroup>,
    child_pid: Option<i32>,
}

impl Fixture {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/private/tmp/hv-r18-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("ws")).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let shim_exe = root.join("bin/host-shim-v1");
        fs::copy(env!("CARGO_BIN_EXE_host-shim"), &shim_exe).unwrap();
        let config = JobConfig {
            prefix: format!("dev.tbhb.hypervisor.test.r18-{}-{n}", std::process::id()),
            uid: process::geteuid().as_raw(),
            throttle_seconds: 1,
        };
        let id = StableId {
            host: "testhost".into(),
            local: "workload".into(),
        };
        let fixture = Self {
            root,
            id,
            config,
            shim_exe,
            job: None,
            groups: vec![],
            child_pid: None,
        };
        fixture
            .driver()
            .create(WorkloadSpec {
                id: fixture.id.clone(),
                runtime: Runtime::Host,
                isolation: Isolation::None,
                image: None,
                mounts: vec![],
                env: vec![],
                credential_refs: vec![],
                network: NetworkPolicy::Host,
                resources: ResourceLimits {
                    memory_bytes: None,
                    cpu_count: None,
                },
                workspace_dir: fixture.root.join("ws"),
                cache_dir: fixture.root.join("cache"),
            })
            .unwrap();
        fixture
    }

    fn driver(&self) -> HostDriver {
        HostDriver::open_launchd(
            &self.root,
            &self.id.host,
            &self.shim_exe,
            self.config.clone(),
        )
        .unwrap()
    }

    fn start(&mut self, driver: &mut HostDriver) -> u32 {
        let service = driver.launchd_service(&self.id).unwrap();
        self.job = Some(JobGuard::new(service));
        driver.start(&self.id).unwrap();
        assert!(self.job.as_ref().unwrap().loaded());
        let pid = driver.shim_pid(&self.id).unwrap();
        self.guard_shim(pid);
        pid
    }

    fn guard_shim(&mut self, pid: u32) {
        // launchd starts each job as its own process group leader.
        self.groups.push(ProcessGroup::new(pid.cast_signed()));
    }

    fn session(&mut self, driver: &HostDriver) -> PathBuf {
        let id = StableId {
            host: self.id.host.clone(),
            local: "session".into(),
        };
        let path = driver
            .spawn_session(
                &self.id,
                id,
                WireSpawnSpec {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "echo $$ > pid; exec sleep 120".into()],
                    env: vec![("PATH".into(), "/bin:/usr/bin".into())],
                    cwd: None,
                    user: None,
                    kind: SessionKind::Shell,
                },
                WireSize { cols: 80, rows: 24 },
            )
            .unwrap();
        let pid = wait_for(|| {
            fs::read_to_string(self.root.join("ws/pid"))
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
        });
        self.groups.push(ProcessGroup::new(pid));
        self.child_pid = Some(pid);
        path
    }

    fn child_alive(&self) -> bool {
        process::test_kill_process_group(Pid::from_raw(self.child_pid.unwrap()).unwrap()).is_ok()
    }

    /// Run a daemon binary that adopts this fixture's shims and prints their state.
    fn adopt(&self, daemon: &Path, shim: &Path) -> String {
        let output = Command::new(daemon)
            .arg("adopt")
            .arg(&self.root)
            .arg(&self.id.host)
            .arg(shim)
            .arg(&self.config.prefix)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut driver) = HostDriver::open_launchd(
            &self.root,
            &self.id.host,
            &self.shim_exe,
            self.config.clone(),
        ) {
            let _ = driver.stop(&self.id);
            let _ = driver.destroy(&self.id);
        }
        // Boot out before killing groups, or launchd would restart a killed shim.
        drop(self.job.take());
        for mut group in self.groups.drain(..) {
            group.kill();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "condition not met"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn open_existing(path: &Path) {
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = OpenRequest {
        versions: vec![1],
        target: OpenTarget::Session("testhost:session".into()),
        mode: Mode::ReadOnly,
        encoding: Encoding::Bytes,
        size: WireSize { cols: 80, rows: 24 },
        client: Capabilities {
            terminal: "test".into(),
            flags: 0,
        },
        resume: None,
        max_frames_per_second: None,
    };
    stream
        .write_all(&Frame::OpenRequest(request).encode().unwrap())
        .unwrap();
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).unwrap();
    let mut frame = vec![0; u32::from_be_bytes(prefix) as usize + 4];
    frame[..4].copy_from_slice(&prefix);
    stream.read_exact(&mut frame[4..]).unwrap();
    assert!(matches!(
        Frame::decode(&frame).unwrap().unwrap().0,
        Frame::OpenResponse(_)
    ));
}

fn parent_pid(pid: u32) -> u32 {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn session_survives_daemon_restart_under_launchd() {
    let mut fixture = Fixture::new();
    let mut driver = fixture.driver();
    let pid = fixture.start(&mut driver);
    let socket = fixture.session(&driver);
    assert_eq!(parent_pid(pid), 1, "launchd does not own the shim");
    drop(driver);

    let daemon = Path::new(env!("CARGO_BIN_EXE_hypervisord"));
    let adopted = fixture.adopt(daemon, &fixture.shim_exe);
    assert_eq!(adopted, format!("testhost:workload Running 1 {pid}\n"));
    assert!(fixture.child_alive());
    open_existing(&socket);
}

#[test]
fn session_survives_daemon_upgrade_under_launchd() {
    let mut fixture = Fixture::new();
    let daemon = fixture.root.join("bin/hypervisord");
    fs::copy(env!("CARGO_BIN_EXE_hypervisord"), &daemon).unwrap();
    let mut driver = fixture.driver();
    let pid = fixture.start(&mut driver);
    let socket = fixture.session(&driver);
    drop(driver);
    let v1 = fixture.shim_exe.clone();
    assert_eq!(
        fixture.adopt(&daemon, &v1),
        format!("testhost:workload Running 1 {pid}\n")
    );

    // Upgrade: install a new shim beside the old one and swap the daemon binary by rename.
    let v2 = fixture.root.join("bin/host-shim-v2");
    fs::copy(&v1, &v2).unwrap();
    let staged = fixture.root.join("bin/hypervisord.new");
    fs::copy(&daemon, &staged).unwrap();
    let old_inode = fs::metadata(&daemon).unwrap().ino();
    fs::rename(&staged, &daemon).unwrap();
    assert_ne!(fs::metadata(&daemon).unwrap().ino(), old_inode);

    assert_eq!(
        fixture.adopt(&daemon, &v2),
        format!("testhost:workload Running 1 {pid}\n")
    );
    let print = Command::new("launchctl")
        .args(["print", &fixture.job.as_ref().unwrap().0])
        .output()
        .unwrap();
    let print = String::from_utf8(print.stdout).unwrap();
    assert!(
        print.contains(&format!("program = {}", v1.display())),
        "the running job no longer names its pinned shim"
    );
    assert!(fixture.child_alive());
    open_existing(&socket);
}

#[test]
fn launchd_restarts_a_crashed_shim_and_the_daemon_reports_it() {
    let mut fixture = Fixture::new();
    let mut driver = fixture.driver();
    let old = fixture.start(&mut driver);
    fixture.session(&driver);
    drop(driver);

    process::kill_process(Pid::from_raw(old.cast_signed()).unwrap(), Signal::KILL).unwrap();
    let mut driver = wait_for(|| {
        let driver = fixture.driver();
        (driver.workloads()[&fixture.id.label()].recovery == Recovery::Restarted).then_some(driver)
    });
    let new = driver.shim_pid(&fixture.id).unwrap();
    fixture.guard_shim(new);
    assert_ne!(new, old);
    assert_eq!(parent_pid(new), 1);
    // The PTY closed with the old shim, so its sessions ended and are not re-adopted.
    assert!(driver.list_sessions(&fixture.id).unwrap().is_empty());
    wait_for(|| (!fixture.child_alive()).then_some(()));
    assert!(
        driver.events(&fixture.id).unwrap()[1].starts_with("shim_restarted_after_unclean_exit:")
    );

    driver.stop(&fixture.id).unwrap();
    assert!(!fixture.job.as_ref().unwrap().loaded());
}
