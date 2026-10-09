//! The daemon and proxy as separate processes over loopback Unix sockets.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{SessionKind, WireSize, WireSpawnSpec};
use hypervisor_core::control::{ClientRequest, ControlError, Cursor, Request, Response};
use hypervisor_core::workload::{
    Isolation, NetworkPolicy, ResourceLimits, Runtime, StableId, WorkloadSpec,
};
use hypervisord::driver::HostDriver;
use process_group::ProcessGroup;

#[path = "../../../tests/support/process_group.rs"]
mod process_group;

const SHIM: &str = env!("CARGO_BIN_EXE_host-shim");

struct Fixture {
    root: PathBuf,
    workload: StableId,
    shim: Option<ProcessGroup>,
    daemon: Option<ProcessGroup>,
    proxy: Option<ProcessGroup>,
}

impl Fixture {
    /// A started host workload with the daemon and the proxy running as separate processes.
    fn new() -> Self {
        let root = PathBuf::from("/private/tmp").join(format!("hv-r23-{}", std::process::id()));
        let (ws, cache) = (root.join("ws"), root.join("cache"));
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let workload = StableId {
            host: "h".into(),
            local: "w".into(),
        };
        let mut fixture = Self {
            root,
            workload: workload.clone(),
            shim: None,
            daemon: None,
            proxy: None,
        };
        let mut driver = HostDriver::open(&fixture.path("drv"), "h", Path::new(SHIM)).unwrap();
        driver
            .create(WorkloadSpec {
                id: workload.clone(),
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
        driver.start(&workload).unwrap();
        let shim = driver.shim_pid(&workload).unwrap().cast_signed();
        fixture.shim = Some(ProcessGroup::new(shim));
        fixture.start_daemon();
        let proxy = env!("CARGO_BIN_EXE_hypervisor-proxy");
        fixture.proxy = Some(launch(&fixture.root, proxy, &["d.sock", "p.sock"]));
        wait_for(&fixture.path("p.sock"));
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn start_daemon(&mut self) {
        let args = ["daemon", "drv", "h", SHIM, "events.log", "d.sock"];
        self.daemon = Some(launch(&self.root, env!("CARGO_BIN_EXE_hypervisord"), &args));
        wait_for(&self.path("d.sock"));
    }

    fn call(&self, request_id: Option<&str>, request: Request) -> Response {
        let mut lines = self.send(&ClientRequest {
            request_id: request_id.map(str::to_owned),
            request,
        });
        let response = next(&mut lines);
        assert!(lines.next().is_none(), "a call has one response");
        response
    }

    fn send(&self, request: &impl serde::Serialize) -> std::io::Lines<BufReader<UnixStream>> {
        let mut stream = UnixStream::connect(self.path("p.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        stream.write_all(&line).unwrap();
        BufReader::new(stream).lines()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.proxy.take();
        self.daemon.take();
        if let Ok(mut driver) = HostDriver::open(&self.path("drv"), "h", Path::new(SHIM)) {
            let _ = driver.stop(&self.workload);
            let _ = driver.destroy(&self.workload);
        }
        self.shim.take();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn launch(dir: &Path, exe: &str, args: &[&str]) -> ProcessGroup {
    let mut child = Command::new(exe)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = ProcessGroup::new(child.id().cast_signed());
    // Reap the child once the fixture kills its group.
    thread::spawn(move || child.wait());
    group
}

fn wait_for(socket: &Path) {
    let start = Instant::now();
    while UnixStream::connect(socket).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{} never answered",
            socket.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn next(lines: &mut std::io::Lines<BufReader<UnixStream>>) -> Response {
    serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
}

fn spawn(workload: &StableId, local: &str, pid_file: &str) -> Request {
    Request::SpawnSession {
        workload: workload.clone(),
        session: StableId {
            host: "h".into(),
            local: local.into(),
        },
        spec: WireSpawnSpec {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), format!("echo $$ > {pid_file}; exec sleep 120")],
            env: vec![("PATH".into(), "/bin:/usr/bin".into())],
            cwd: None,
            user: None,
            kind: SessionKind::Shell,
        },
        size: WireSize { cols: 80, rows: 24 },
    }
}

fn event_seq(response: &Response) -> u64 {
    match response {
        Response::Event(event) => event.cursor.seq,
        other => panic!("expected an event, got {other:?}"),
    }
}

/// Spawn, retry, detail, and kill through the proxy while a watcher follows the log.
fn first_daemon(fixture: &Fixture) -> (Response, Request, Response) {
    let workload = &fixture.workload;
    assert_eq!(
        fixture.call(None, Request::ListWorkloads),
        Response::Workloads(vec![workload.clone()])
    );
    let mut watch = fixture.send(&ClientRequest {
        request_id: None,
        request: Request::Watch { after: None },
    });

    // A retried spawn replays the first outcome; a reused ID with another request is refused.
    let spawned = fixture.call(Some("spawn-1"), spawn(workload, "s", "pid"));
    let Response::Spawned { socket, cursor } = spawned.clone() else {
        panic!("spawn failed: {spawned:?}");
    };
    assert_eq!(cursor.seq, 1);
    let pid: i32 = loop {
        if let Some(pid) = fs::read_to_string(fixture.path("ws/pid"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            break pid;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let _session = ProcessGroup::new(pid);
    assert_eq!(
        fixture.call(Some("spawn-1"), spawn(workload, "s", "pid")),
        spawned
    );
    assert_eq!(
        fixture.call(Some("spawn-1"), spawn(workload, "other", "pid2")),
        Response::Error(ControlError::RequestIdReused)
    );
    assert_eq!(
        fixture.call(None, spawn(workload, "other", "pid2")),
        Response::Error(ControlError::MissingRequestId)
    );
    assert_eq!(event_seq(&next(&mut watch)), 1);

    let session = StableId {
        host: "h".into(),
        local: "s".into(),
    };
    let detail = Request::SessionDetail {
        workload: workload.clone(),
        session: session.clone(),
    };
    let Response::Detail(detail) = fixture.call(None, detail) else {
        panic!("no session detail");
    };
    assert_eq!((detail.live, detail.socket), (true, Some(socket)));
    let uid = rustix::process::geteuid().as_raw();
    assert_eq!(detail.spawned_by.map(|operator| operator.uid), Some(uid));

    let kill = Request::KillSession {
        workload: workload.clone(),
        session,
    };
    let killed = fixture.call(Some("kill-1"), kill.clone());
    assert!(matches!(&killed, Response::Killed { cursor } if cursor.seq == 2));
    assert_eq!(
        event_seq(&next(&mut watch)),
        2,
        "watch follows live appends"
    );
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "kill ends the session"
    );
    (spawned, kill, killed)
}

#[test]
fn proxy_forwards_idempotent_calls_and_the_log_resumes_across_daemon_restarts() {
    let mut fixture = Fixture::new();
    let (spawned, kill, killed) = first_daemon(&fixture);
    let workload = fixture.workload.clone();
    let sessions = Request::ListSessions {
        workload: workload.clone(),
    };
    assert_eq!(
        fixture.call(None, sessions.clone()),
        Response::Sessions(vec![])
    );

    // Restart the daemon behind the running proxy, after a torn append to its log.
    fixture.daemon.take();
    let log = fixture.path("events.log");
    let intact = fs::metadata(&log).unwrap().len();
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    file.write_all(b"{\"cursor\":").unwrap();
    fs::remove_file(fixture.path("d.sock")).unwrap();
    fixture.start_daemon();
    assert_eq!(fs::metadata(&log).unwrap().len(), intact);

    let after = Cursor {
        host: "h".into(),
        seq: 1,
    };
    let mut resumed = fixture.send(&ClientRequest {
        request_id: None,
        request: Request::Watch { after: Some(after) },
    });
    assert_eq!(
        event_seq(&next(&mut resumed)),
        2,
        "resume yields only unseen events"
    );
    // Replays come from the restored log; the session is not spawned again.
    assert_eq!(
        fixture.call(Some("spawn-1"), spawn(&workload, "s", "pid")),
        spawned
    );
    assert_eq!(fixture.call(Some("kill-1"), kill), killed);
    assert_eq!(fixture.call(None, sessions), Response::Sessions(vec![]));
    let foreign = Cursor {
        host: "elsewhere".into(),
        seq: 1,
    };
    let foreign = Request::Watch {
        after: Some(foreign),
    };
    assert_eq!(
        fixture.call(None, foreign),
        Response::Error(ControlError::ForeignCursor)
    );

    // A client cannot claim an operator; the proxy refuses the extra field.
    let claimed = serde_json::json!({"request_id": null, "request": "list_workloads", "operator": {"uid": 0, "gid": 0}});
    assert!(matches!(
        next(&mut fixture.send(&claimed)),
        Response::Error(ControlError::Failed(_))
    ));
}
