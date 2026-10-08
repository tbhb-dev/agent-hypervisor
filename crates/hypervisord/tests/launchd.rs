//! Host shims under launchd: registration, start paths, and test job cleanup.
//!
//! Every job lives in the user's `gui/<uid>` domain under a `dev.tbhb.hypervisor.test.`
//! label naming the owning test PID, and loads from a temporary plist. `JobGuard` boots it
//! out on return or panic, the SIGTERM handler on SIGTERM, and the next run's sweep after
//! SIGKILL.
#![cfg(target_os = "macos")]

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::launchd::{self, JobConfig};
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, Recovery, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::driver::HostDriver;
use rustix::process::{self, Pid, Signal};

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

use process_group::ProcessGroup;

static NEXT: AtomicU64 = AtomicU64::new(0);
static SERIAL: Mutex<()> = Mutex::new(());
static TERMINATED: AtomicBool = AtomicBool::new(false);

/// Serialize launchd tests and boot out jobs left by dead test runs before registering any.
fn serial() -> MutexGuard<'static, ()> {
    static HANDLER: Once = Once::new();
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    HANDLER.call_once(handle_sigterm);
    sweep(false);
    guard
}

/// Boot out every test job whose owner is dead, or owned by this process when `exiting`.
fn sweep(exiting: bool) {
    let domain = format!("gui/{}", process::geteuid().as_raw());
    let output = Command::new("launchctl")
        .args(["print", &domain])
        .output()
        .unwrap();
    assert!(output.status.success(), "launchctl print {domain} failed");
    let jobs = launchd::test_jobs(&String::from_utf8_lossy(&output.stdout));
    let me = std::process::id();
    let live: BTreeSet<u32> = jobs
        .values()
        .copied()
        .filter(|&owner| {
            (!exiting || owner != me)
                && !matches!(
                    process::test_kill_process(Pid::from_raw(owner.cast_signed()).unwrap()),
                    Err(rustix::io::Errno::SRCH)
                )
        })
        .collect();
    for label in launchd::sweep_targets(&jobs, &live) {
        let _ = launchctl(&["bootout", &format!("{domain}/{label}")]).status();
    }
}

extern "C" fn on_sigterm(_: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// The test harness does not unwind on a signal, so a watcher boots out this process's jobs.
fn handle_sigterm() {
    let handler: extern "C" fn(libc::c_int) = on_sigterm;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    #[expect(unsafe_code, reason = "install a SIGTERM handler for job cleanup")]
    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
    thread::spawn(|| {
        loop {
            if TERMINATED.load(Ordering::SeqCst) {
                sweep(true);
                std::process::exit(143);
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
}

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
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let serial = serial();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/private/tmp/hv-r18-{}-{n}", std::process::id()));
        fs::create_dir_all(root.join("ws")).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let shim_exe = root.join("bin/host-shim-v1");
        fs::copy(env!("CARGO_BIN_EXE_host-shim"), &shim_exe).unwrap();
        let config = JobConfig {
            prefix: launchd::test_prefix(std::process::id(), n),
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
            _serial: serial,
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
        // Keep an existing guard: replacing it would boot the job out before the driver could.
        if self.job.is_none() {
            self.job = Some(JobGuard::new(driver.launchd_service(&self.id).unwrap()));
        }
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

fn control_socket(root: &Path) -> PathBuf {
    let metadata = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    metadata.with_extension("sock")
}

fn process_alive(pid: u32) -> bool {
    process::test_kill_process(Pid::from_raw(pid.cast_signed()).unwrap()).is_ok()
}

#[test]
fn attended_start_after_a_dead_shim_is_adopted_as_running() {
    let mut fixture = Fixture::new();
    let mut driver = fixture.driver();
    let old = fixture.start(&mut driver);
    drop(driver);
    // bootout sends SIGTERM, which the shim does not handle, so its socket stays behind.
    let service = fixture.job.as_ref().unwrap().0.clone();
    assert!(
        launchctl(&["bootout", &service])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| (!process_alive(old)).then_some(()));
    assert!(control_socket(&fixture.root).exists());

    let mut driver = fixture.driver();
    assert_eq!(
        driver.workloads()[&fixture.id.label()].recovery,
        Recovery::Stopped
    );
    let pid = fixture.start(&mut driver);
    drop(driver);
    let daemon = Path::new(env!("CARGO_BIN_EXE_hypervisord"));
    assert_eq!(
        fixture.adopt(daemon, &fixture.shim_exe),
        format!("testhost:workload Running 0 {pid}\n")
    );
}

#[test]
fn start_replaces_a_loaded_job_whose_shim_is_silent() {
    let mut fixture = Fixture::new();
    let mut driver = fixture.driver();
    let old = fixture.start(&mut driver);
    drop(driver);
    process::kill_process(Pid::from_raw(old.cast_signed()).unwrap(), Signal::STOP).unwrap();

    let mut driver = fixture.driver();
    assert_eq!(
        driver.workloads()[&fixture.id.label()].recovery,
        Recovery::Stopped
    );
    assert!(fixture.job.as_ref().unwrap().loaded());
    let new = fixture.start(&mut driver);
    assert_ne!(new, old);
    assert_eq!(parent_pid(new), 1);
    assert_eq!(
        driver.workloads()[&fixture.id.label()].recovery,
        Recovery::Running
    );
    wait_for(|| (!process_alive(old)).then_some(()));
}

/// Child half of the killed-run tests: does nothing unless the parent test spawned it.
#[test]
fn killed_run_child_registers_a_job_and_waits() {
    let Some(ready) = std::env::var_os("HV_LAUNCHD_CHILD_READY") else {
        return;
    };
    let mut fixture = Fixture::new();
    let mut driver = fixture.driver();
    let pid = fixture.start(&mut driver);
    let staged = PathBuf::from(&ready).with_extension("tmp");
    let job = &fixture.job.as_ref().unwrap().0;
    fs::write(
        &staged,
        format!("{job}\n{pid}\n{}\n", fixture.root.display()),
    )
    .unwrap();
    fs::rename(staged, ready).unwrap();
    thread::sleep(Duration::from_secs(60));
}

/// Kill a child test run after it registers a job; return guards for the leaked job.
fn kill_child_run(signal: Signal) -> (ProcessGroup, JobGuard, u32, PathBuf) {
    let ready = PathBuf::from(format!(
        "/private/tmp/hv-r18-child-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "killed_run_child_registers_a_job_and_waits"])
        .env("HV_LAUNCHD_CHILD_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lines = wait_for(|| fs::read_to_string(&ready).ok());
    let _ = fs::remove_file(&ready);
    let lines: Vec<&str> = lines.lines().collect();
    let shim: u32 = lines[1].parse().unwrap();
    // Declared before the job guard, the group is killed only after the job is booted out.
    let group = ProcessGroup::new(shim.cast_signed());
    let job = JobGuard::new(lines[0].into());
    process::kill_process(Pid::from_raw(child.id().cast_signed()).unwrap(), signal).unwrap();
    child.wait().unwrap();
    (group, job, shim, PathBuf::from(lines[2]))
}

#[test]
fn next_run_sweeps_the_job_of_a_sigkilled_test_run() {
    let first_run = serial();
    let (_group, job, shim, root) = kill_child_run(Signal::KILL);
    assert!(
        job.loaded(),
        "SIGKILL ran no cleanup, so the job is still loaded"
    );
    assert!(process_alive(shim));
    drop(first_run);
    let _next_run = serial();
    assert!(!job.loaded());
    wait_for(|| (!process_alive(shim)).then_some(()));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_sigtermed_test_run_boots_out_its_own_job() {
    let _serial = serial();
    let (_group, job, shim, root) = kill_child_run(Signal::TERM);
    assert!(!job.loaded());
    wait_for(|| (!process_alive(shim)).then_some(()));
    let _ = fs::remove_dir_all(root);
}
