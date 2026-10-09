//! Apple container integration; run only by check:host on an unsandboxed macOS host.
#![cfg(target_os = "macos")]

use std::fs::{self, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{
    Capabilities, Encoding, Frame, Mode, OpenRequest, OpenTarget, SessionKind, WireSize,
    WireSpawnSpec,
};
use hypervisor_core::container;
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::container_driver::ContainerDriver;

struct Fixture {
    root: PathBuf,
    id: StableId,
}

impl Fixture {
    fn new() -> (Self, ContainerDriver) {
        let root = PathBuf::from(format!("/private/tmp/hv-r20-{}", std::process::id()));
        let ws = root.join("ws");
        let cache = root.join("cache");
        fs::create_dir_all(&ws).unwrap();
        fs::set_permissions(&ws, Permissions::from_mode(0o777)).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let id = StableId {
            host: "r20".into(),
            local: std::process::id().to_string(),
        };
        let agent = std::env::current_dir()
            .unwrap()
            .join("target/aarch64-unknown-linux-musl/debug/host-shim");
        assert!(
            agent.is_file(),
            "build guest agent with mise run guest:build first"
        );
        let mut driver = ContainerDriver::open(&root.join("rt"), &id.host, &agent).unwrap();
        let fixture = Self { root, id };
        driver
            .create(WorkloadSpec {
                id: fixture.id.clone(),
                runtime: Runtime::AppleContainer,
                isolation: Isolation::VirtualMachine,
                image: Some("alpine:3.21.3".into()),
                mounts: vec![],
                env: vec![],
                credential_refs: vec![],
                network: NetworkPolicy::Isolated,
                resources: ResourceLimits {
                    memory_bytes: Some(512 * 1024 * 1024),
                    cpu_count: Some(2),
                },
                workspace_dir: ws,
                cache_dir: cache,
            })
            .unwrap();
        (fixture, driver)
    }

    fn session(
        &self,
        driver: &mut ContainerDriver,
        local: &str,
        user: &str,
        script: &str,
    ) -> PathBuf {
        driver
            .spawn_session(
                &self.id,
                StableId {
                    host: self.id.host.clone(),
                    local: local.into(),
                },
                WireSpawnSpec {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    env: vec![("PATH".into(), "/bin:/usr/bin".into())],
                    cwd: None,
                    user: Some(user.into()),
                    kind: SessionKind::Shell,
                },
                WireSize { cols: 80, rows: 24 },
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let name = container::name(&self.id);
        let container = Command::new("container")
            .args(["delete", "--force", &name])
            .stdin(Stdio::null())
            .output();
        let volume = Command::new("container")
            .args(["volume", "delete", &container::cache_volume(&self.id)])
            .stdin(Stdio::null())
            .output();
        let files = fs::remove_dir_all(&self.root);
        eprintln!("fixture cleanup: container={container:?} volume={volume:?} files={files:?}");
    }
}

fn exec(name: &str, program: &str, args: &[&str]) -> std::process::Output {
    Command::new("container")
        .arg("exec")
        .arg(name)
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "guest condition did not become true"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn open_terminal(path: &PathBuf, id: &str) {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = OpenRequest {
        versions: vec![1],
        target: OpenTarget::Session(id.into()),
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

fn assert_private_sockets(root: &Path) {
    let mut socket_count = 0;
    for entry in fs::read_dir(root.join("rt")).unwrap() {
        let path = entry.unwrap().path();
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("sock" | "c")
        ) {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            socket_count += 1;
        }
    }
    assert_eq!(socket_count, 3);
}

#[test]
#[ignore = "requires Apple container and an unsandboxed macOS host; check:host builds the guest first"]
fn container_guest_sessions_reviewer_and_cleanup() {
    let (fixture, mut driver) = Fixture::new();
    driver.start(&fixture.id).unwrap();
    let name = container::name(&fixture.id);
    let inspected = Command::new("container")
        .args(["inspect", &name])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let details: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let disk = details
        .get(0)
        .and_then(|entry| entry.get("diskSize"))
        .and_then(serde_json::Value::as_u64);
    let volume = Command::new("container")
        .args(["volume", "inspect", &container::cache_volume(&fixture.id)])
        .output()
        .unwrap();
    assert!(volume.status.success());
    let volume_details: serde_json::Value = serde_json::from_slice(&volume.stdout).unwrap();
    let volume_disk = volume_details
        .get(0)
        .and_then(|entry| entry.get("configuration"))
        .and_then(|config| config.get("sizeInBytes"))
        .and_then(serde_json::Value::as_u64);
    eprintln!(
        "test VM count=1 cpus=2 memory_bytes=536870912 disk_bytes={disk:?} cache_volume_bytes={volume_disk:?} container={name}"
    );
    let route = exec(&name, "/bin/cat", &["/proc/net/route"]);
    assert!(route.status.success());
    assert!(
        !String::from_utf8_lossy(&route.stdout).contains("00000000"),
        "guest has a default route"
    );
    let writer = fixture.session(
        &mut driver,
        "writer",
        "10001",
        "id -u >/workspace/uid; echo ok >/workspace/writer; mkdir /workspace/owned; echo ok >/workspace/owned/readme; exec sleep 120",
    );
    wait_for(|| fixture.root.join("ws/writer").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("ws/uid"))
            .unwrap()
            .trim(),
        "10001"
    );
    open_terminal(&writer, "r20:writer");
    let reviewer = fixture.session(&mut driver, "reviewer", "reviewer:10002",
        "cat /workspace/writer >/cache/users/10002/read; cat /workspace/owned/readme >/cache/users/10002/subdir_read; if echo bad >/workspace/forbidden; then echo bad >/cache/users/10002/result; else echo denied >/cache/users/10002/result; fi; if echo bad >/workspace/owned/forbidden; then echo bad >/cache/users/10002/subdir_result; else echo denied >/cache/users/10002/subdir_result; fi; echo \"$GIT_OPTIONAL_LOCKS\" >/cache/users/10002/gitlock; exec sleep 120");
    wait_for(|| {
        exec(&name, "/bin/cat", &["/cache/users/10002/result"])
            .status
            .success()
    });
    assert!(!fixture.root.join("ws/forbidden").exists());
    assert!(!fixture.root.join("ws/owned/forbidden").exists());
    for (file, expected) in [
        ("result", "denied"),
        ("subdir_result", "denied"),
        ("read", "ok"),
        ("subdir_read", "ok"),
        ("gitlock", "0"),
    ] {
        let output = exec(&name, "/bin/cat", &[&format!("/cache/users/10002/{file}")]);
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }
    assert_private_sockets(&fixture.root);
    open_terminal(&reviewer, "r20:reviewer");
    drop(driver);
    let agent = std::env::current_dir()
        .unwrap()
        .join("target/aarch64-unknown-linux-musl/debug/host-shim");
    let mut adopted =
        ContainerDriver::open(&fixture.root.join("rt"), &fixture.id.host, &agent).unwrap();
    assert_eq!(adopted.list_sessions(&fixture.id).unwrap().len(), 2);
    open_terminal(&reviewer, "r20:reviewer");
    adopted.stop(&fixture.id).unwrap();
    adopted.destroy(&fixture.id).unwrap();
    assert!(
        !Command::new("container")
            .args(["inspect", &name])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !Command::new("container")
            .args(["volume", "inspect", &container::cache_volume(&fixture.id)])
            .output()
            .unwrap()
            .status
            .success()
    );
    eprintln!("container and cache volume cleaned: {name}");
}
