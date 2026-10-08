//! Host shim lifecycle and daemon-side recovery.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{
    Capabilities, Encoding, Frame, Mode, OpenRequest, OpenTarget, SessionKind, WireSize,
    WireSpawnSpec,
};
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, Recovery, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::driver::HostDriver;

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    id: StableId,
    shim: Option<process_group::ProcessGroup>,
    child: Option<process_group::ProcessGroup>,
}

impl Fixture {
    fn new() -> (Self, HostDriver) {
        #[cfg(target_os = "macos")]
        let temp = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let temp = std::env::temp_dir();
        let root = temp.join(format!(
            "hv-r17-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let ws = root.join("ws");
        let cache = root.join("cache");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let id = StableId {
            host: "testhost".into(),
            local: "workload".into(),
        };
        let mut driver = HostDriver::open(&root, &id.host, shim_exe()).unwrap();
        driver
            .create(WorkloadSpec {
                id: id.clone(),
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
                workspace_dir: ws,
                cache_dir: cache,
            })
            .unwrap();
        (
            Self {
                root,
                id,
                shim: None,
                child: None,
            },
            driver,
        )
    }

    fn start(&mut self, driver: &mut HostDriver) {
        driver.start(&self.id).unwrap();
        self.shim = Some(process_group::ProcessGroup::new(
            driver.shim_pid(&self.id).unwrap().cast_signed(),
        ));
    }

    fn session(&mut self, driver: &HostDriver) -> PathBuf {
        let pid_file = self.root.join("ws/pid");
        let id = StableId {
            host: self.id.host.clone(),
            local: "session".into(),
        };
        let path = driver
            .spawn_session(
                &self.id,
                id.clone(),
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
        let start = Instant::now();
        while !pid_file.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "session never wrote its PID"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let pid = fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        self.child = Some(process_group::ProcessGroup::new(pid));
        assert_eq!(driver.list_sessions(&self.id).unwrap(), vec![id]);
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut driver) = HostDriver::open(&self.root, &self.id.host, shim_exe()) {
            let _ = driver.stop(&self.id);
            let _ = driver.destroy(&self.id);
        }
        if let Some(mut group) = self.child.take() {
            group.kill();
        }
        if let Some(mut group) = self.shim.take() {
            group.kill();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn shim_exe() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_host-shim"))
}

fn open_existing(path: &std::path::Path) {
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

#[test]
fn starts_spawns_and_stops_with_a_terminal_socket() {
    let (mut fixture, mut driver) = Fixture::new();
    fixture.start(&mut driver);
    let socket = fixture.session(&driver);
    open_existing(&socket);
    assert_eq!(driver.stats(&fixture.id).unwrap(), 1);
    assert_eq!(driver.events(&fixture.id).unwrap().len(), 2);
    driver.stop(&fixture.id).unwrap();
    assert_eq!(
        driver.workloads()[&fixture.id.label()].recovery,
        Recovery::Stopped
    );
}

#[test]
fn new_daemon_adopts_running_shim_and_session() {
    let (mut fixture, mut driver) = Fixture::new();
    fixture.start(&mut driver);
    let socket = fixture.session(&driver);
    drop(driver);
    let recovered = HostDriver::open(&fixture.root, &fixture.id.host, shim_exe()).unwrap();
    assert_eq!(
        recovered.workloads()[&fixture.id.label()].recovery,
        Recovery::Running
    );
    assert_eq!(recovered.list_sessions(&fixture.id).unwrap().len(), 1);
    open_existing(&socket);
}

#[test]
fn dead_shim_while_daemon_is_down_is_stopped() {
    let (mut fixture, mut driver) = Fixture::new();
    fixture.start(&mut driver);
    fixture.session(&driver);
    drop(driver);
    fixture.shim.as_mut().unwrap().kill();
    let start = Instant::now();
    loop {
        let recovered = HostDriver::open(&fixture.root, &fixture.id.host, shim_exe()).unwrap();
        if recovered.workloads()[&fixture.id.label()].recovery == Recovery::Stopped {
            let mut recovered = recovered;
            recovered.start(&fixture.id).unwrap();
            fixture.shim = Some(process_group::ProcessGroup::new(
                recovered.shim_pid(&fixture.id).unwrap().cast_signed(),
            ));
            assert!(recovered.list_sessions(&fixture.id).unwrap().is_empty());
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "dead shim still responded"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
