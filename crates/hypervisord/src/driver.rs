//! Detached host workload shims and their private control sockets.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hypervisor_core::channel::{WireSize, WireSpawnSpec};
use hypervisor_core::emulator::Size;
use hypervisor_core::session::{HolderConfig, Persistence, SessionKind, SpawnSpec};
use hypervisor_core::workload::{
    self, HostDecision, HostRequest as Request, HostResponse as Response, Recovery, StableId,
    WorkloadSpec,
};
use hypervisor_ghostty::GhosttyEmulator;
use hypervisor_pty::UnixSpawner;
use hypervisor_session::{Command, LogContext, spawn};
use rustix::process::{self, Pid, Signal};
use sha2::{Digest, Sha256};

use crate::terminal_socket::TerminalSocket;

const MAX_CONTROL: usize = 1024 * 1024;
const START_WAIT: Duration = Duration::from_secs(5);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

/// A persisted workload and its observed shim state.
#[derive(Debug)]
pub struct Workload {
    pub spec: WorkloadSpec,
    pub recovery: Recovery,
}

/// The daemon-side unsandboxed driver; dropping it leaves shims running.
pub struct HostDriver {
    root: PathBuf,
    host: String,
    shim_exe: PathBuf,
    workloads: BTreeMap<String, Workload>,
}

impl HostDriver {
    /// Open the private registry and reconcile every saved workload by socket handshake.
    ///
    /// # Errors
    /// Invalid registry, metadata, or I/O.
    pub fn open(root: &Path, host: &str, shim_exe: &Path) -> io::Result<Self> {
        StableId {
            host: host.into(),
            local: "probe".into(),
        }
        .validate()
        .map_err(invalid)?;
        fs::create_dir_all(root)?;
        fs::set_permissions(root, Permissions::from_mode(0o700))?;
        let root = fs::canonicalize(root)?;
        if root.as_os_str().as_bytes().len() + 30 > 103 {
            return Err(invalid("runtime root is too long for Unix sockets"));
        }
        let mut driver = Self {
            root,
            host: host.into(),
            shim_exe: shim_exe.into(),
            workloads: BTreeMap::new(),
        };
        for entry in fs::read_dir(&driver.root)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let spec: WorkloadSpec = serde_json::from_reader(File::open(entry.path())?)?;
            workload::validate_host(&spec).map_err(invalid)?;
            if entry.path() != driver.metadata(&spec.id) {
                return Err(invalid("workload metadata filename does not match its ID"));
            }
            let id = spec.id.clone();
            driver.workloads.insert(
                id.label(),
                Workload {
                    spec,
                    recovery: Recovery::Stopped,
                },
            );
            let found = match driver.call(&id, &Request::Ping) {
                Ok(Response::Identity(found)) => Some(found),
                _ => None,
            };
            if let Some(saved) = driver.workloads.get_mut(&id.label()) {
                saved.recovery = workload::recovery_identity(&driver.host, &id, found.as_ref());
            }
        }
        Ok(driver)
    }

    #[must_use]
    pub fn workloads(&self) -> &BTreeMap<String, Workload> {
        &self.workloads
    }

    /// Persist a workload before starting its shim.
    ///
    /// # Errors
    /// Invalid or duplicate workload, or filesystem failure.
    pub fn create(&mut self, mut spec: WorkloadSpec) -> io::Result<()> {
        workload::admit_host_workload(&self.host, &spec).map_err(invalid)?;
        spec.workspace_dir = fs::canonicalize(&spec.workspace_dir)?;
        spec.cache_dir = fs::canonicalize(&spec.cache_dir)?;
        workload::admit_host_workload(&self.host, &spec).map_err(invalid)?;
        let path = self.metadata(&spec.id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        serde_json::to_writer(&mut file, &spec)?;
        file.flush()?;
        self.workloads.insert(
            spec.id.label(),
            Workload {
                spec,
                recovery: Recovery::Stopped,
            },
        );
        Ok(())
    }

    /// Start a detached shim and confirm its ID over the private control socket.
    ///
    /// # Errors
    /// Unknown or already running workload, spawn failure, or handshake timeout.
    pub fn start(&mut self, id: &StableId) -> io::Result<()> {
        let workload = self
            .workloads
            .get(&id.label())
            .ok_or_else(|| invalid("unknown workload"))?;
        let socket_answers = matches!(self.call(id, &Request::Ping), Ok(Response::Identity(_)));
        workload::admit_start(workload.recovery, socket_answers).map_err(invalid)?;
        let mut command = ProcessCommand::new(&self.shim_exe);
        command
            .arg("shim")
            .arg(self.metadata(id))
            .arg(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe in the child before exec.
        #[expect(unsafe_code, reason = "detach the shim from the daemon process group")]
        unsafe {
            command.pre_exec(|| {
                process::setsid()?;
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = Pid::from_child(&child);
        let started = Instant::now();
        while started.elapsed() < START_WAIT {
            if matches!(self.call(id, &Request::Ping), Ok(Response::Identity(found)) if found == *id)
            {
                if let Some(saved) = self.workloads.get_mut(&id.label()) {
                    saved.recovery = Recovery::Running;
                }
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            if child.try_wait()?.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = process::kill_process_group(pid, Signal::KILL);
        let _ = child.wait();
        Err(io::Error::other(
            "shim did not complete its identity handshake",
        ))
    }

    /// Ask a running shim to close its sessions and exit.
    ///
    /// # Errors
    /// Unknown workload or failed control exchange.
    pub fn stop(&mut self, id: &StableId) -> io::Result<()> {
        if !matches!(self.call(id, &Request::Stop)?, Response::Stopped) {
            return Err(io::Error::other("shim refused stop"));
        }
        let started = Instant::now();
        while self.socket(id).exists() {
            if started.elapsed() > Duration::from_secs(70) {
                return Err(io::Error::other("shim did not finish stopping"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.workloads
            .get_mut(&id.label())
            .ok_or_else(|| invalid("unknown workload"))?
            .recovery = Recovery::Stopped;
        Ok(())
    }

    /// Remove stopped workload metadata.
    ///
    /// # Errors
    /// The shim is still running or metadata cannot be removed.
    pub fn destroy(&mut self, id: &StableId) -> io::Result<()> {
        if matches!(self.call(id, &Request::Ping), Ok(Response::Identity(found)) if found == *id) {
            return Err(invalid("stop the workload before destroy"));
        }
        fs::remove_file(self.metadata(id))?;
        self.workloads.remove(&id.label());
        Ok(())
    }

    /// Spawn a session in the shim, returning its existing terminal channel path.
    ///
    /// # Errors
    /// Shim refusal or control I/O failure.
    pub fn spawn_session(
        &self,
        workload: &StableId,
        id: StableId,
        spec: WireSpawnSpec,
        size: WireSize,
    ) -> io::Result<PathBuf> {
        match self.call(workload, &Request::Spawn { id, spec, size })? {
            Response::SessionSocket(path) => Ok(path),
            Response::Error(error) => Err(io::Error::other(error)),
            _ => Err(io::Error::other("unexpected shim response")),
        }
    }

    /// List stable session IDs currently held by the shim.
    ///
    /// # Errors
    /// Control I/O failure.
    pub fn list_sessions(&self, id: &StableId) -> io::Result<Vec<StableId>> {
        match self.call(id, &Request::List)? {
            Response::Sessions(sessions) => Ok(sessions),
            _ => Err(io::Error::other("unexpected shim response")),
        }
    }

    /// Return the shim's session count.
    ///
    /// # Errors
    /// Control I/O failure.
    pub fn stats(&self, id: &StableId) -> io::Result<usize> {
        match self.call(id, &Request::Stats)? {
            Response::Stats { sessions, .. } => Ok(sessions),
            _ => Err(io::Error::other("unexpected shim response")),
        }
    }

    /// PID for supervision diagnostics, never used as the workload identity.
    ///
    /// # Errors
    /// Control I/O failure.
    pub fn shim_pid(&self, id: &StableId) -> io::Result<u32> {
        match self.call(id, &Request::Stats)? {
            Response::Stats { pid, .. } => Ok(pid),
            _ => Err(io::Error::other("unexpected shim response")),
        }
    }

    /// Return the shim's in-memory lifecycle events.
    ///
    /// # Errors
    /// Control I/O failure.
    pub fn events(&self, id: &StableId) -> io::Result<Vec<String>> {
        match self.call(id, &Request::Events)? {
            Response::Events(events) => Ok(events),
            _ => Err(io::Error::other("unexpected shim response")),
        }
    }

    fn metadata(&self, id: &StableId) -> PathBuf {
        self.root.join(format!("{}.json", label_hash(id)))
    }
    fn socket(&self, id: &StableId) -> PathBuf {
        self.root.join(format!("{}.sock", label_hash(id)))
    }

    fn call(&self, id: &StableId, request: &Request) -> io::Result<Response> {
        if !self.workloads.contains_key(&id.label()) {
            return Err(invalid("unknown workload"));
        }
        let mut stream = UnixStream::connect(self.socket(id))?;
        stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
        stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
        serde_json::to_writer(&mut stream, request)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        serde_json::from_reader(stream).map_err(Into::into)
    }
}

struct HostedSession {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

struct ShimState {
    sessions: BTreeMap<String, (StableId, HostedSession)>,
    events: Vec<String>,
    stopping: bool,
    stop_replied: bool,
}

/// Run the shim process. The caller passes paths from its private registry.
///
/// # Errors
/// Invalid metadata or control I/O.
pub fn serve(metadata: &Path, root: &Path) -> io::Result<()> {
    let spec: WorkloadSpec = serde_json::from_reader(File::open(metadata)?)?;
    workload::validate_host(&spec).map_err(invalid)?;
    let socket = root.join(format!("{}.sock", label_hash(&spec.id)));
    // A stale socket from a dead shim is removed only after connection fails.
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(invalid("shim already running"));
        }
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let state = Arc::new(Mutex::new(ShimState {
        sessions: BTreeMap::new(),
        events: vec![format!("workload_started:{}", spec.id.label())],
        stopping: false,
        stop_replied: false,
    }));
    let spec = Arc::new(spec);
    loop {
        if state
            .lock()
            .map_err(|_| io::Error::other("shim state poisoned"))?
            .stop_replied
        {
            break;
        }
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => {
                eprintln!("host shim accept failed: {error}");
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        if let Err(error) = stream.set_nonblocking(false) {
            eprintln!("host shim control setup failed: {error}");
            continue;
        }
        let state = Arc::clone(&state);
        let spec = Arc::clone(&spec);
        let root = root.to_path_buf();
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        thread::spawn(move || {
            if let Err(error) = exchange(&mut stream, &root, &spec, &state, deadline) {
                eprintln!("host shim control exchange failed: {error}");
            }
        });
    }
    let sessions = std::mem::take(
        &mut state
            .lock()
            .map_err(|_| io::Error::other("shim state poisoned"))?
            .sessions,
    );
    for (_, (_, hosted)) in sessions {
        hosted.stop.store(true, Ordering::Relaxed);
        let _ = hosted.thread.join();
    }
    fs::remove_file(socket)?;
    Ok(())
}

fn exchange(
    stream: &mut UnixStream,
    root: &Path,
    spec: &WorkloadSpec,
    state: &Mutex<ShimState>,
    deadline: Instant,
) -> io::Result<()> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control request timed out",
            ));
        }
        stream.set_read_timeout(Some(remaining))?;
        let mut chunk = [0; 8192];
        let limit = (MAX_CONTROL + 1 - bytes.len()).min(chunk.len());
        match stream.read(&mut chunk[..limit])? {
            0 => break,
            count => bytes.extend_from_slice(&chunk[..count]),
        }
        if bytes.len() > MAX_CONTROL {
            return Err(invalid("control request too large"));
        }
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    let response = {
        let mut state = state
            .lock()
            .map_err(|_| io::Error::other("shim state poisoned"))?;
        if state.stopping {
            return Ok(());
        }
        let ids: Vec<_> = state.sessions.values().map(|(id, _)| id.clone()).collect();
        let decision =
            workload::control_decision(request, &spec.id, &ids, std::process::id(), &state.events);
        match decision {
            HostDecision::Reply(reply) => reply,
            HostDecision::Spawn {
                id,
                spec: session,
                size,
            } => match start_session(root, spec, id, session, size, &state.sessions) {
                Ok((id, hosted)) => {
                    let path = hosted.path.clone();
                    state.events.push(format!("session_created:{}", id.label()));
                    state.sessions.insert(id.label(), (id, hosted));
                    Response::SessionSocket(path)
                }
                Err(error) => Response::Error(error.to_string()),
            },
            HostDecision::Stop => {
                state.stopping = true;
                Response::Stopped
            }
        }
    };
    let stopped = matches!(response, Response::Stopped);
    let result = stream
        .set_write_timeout(Some(CONTROL_TIMEOUT))
        .and_then(|()| serde_json::to_writer(stream, &response).map_err(Into::into));
    if stopped {
        state
            .lock()
            .map_err(|_| io::Error::other("shim state poisoned"))?
            .stop_replied = true;
    }
    result
}

fn start_session(
    root: &Path,
    workload: &WorkloadSpec,
    id: StableId,
    wire: WireSpawnSpec,
    wire_size: WireSize,
    sessions: &BTreeMap<String, (StableId, HostedSession)>,
) -> io::Result<(StableId, HostedSession)> {
    workload::admit_session(&workload.id, &id, sessions.contains_key(&id.label()))
        .map_err(invalid)?;
    let size =
        Size::new(wire_size.cols, wire_size.rows).map_err(|_| invalid("zero terminal size"))?;
    let kind = match wire.kind {
        hypervisor_core::channel::SessionKind::Agent => SessionKind::Agent,
        hypervisor_core::channel::SessionKind::Shell => SessionKind::Shell,
    };
    let mut spec = SpawnSpec::new(wire.command, size, kind);
    spec.args = wire.args;
    spec.env = workload::session_env(workload, &wire.env);
    spec.cwd = Some(
        wire.cwd
            .map_or_else(|| workload.workspace_dir.clone(), PathBuf::from),
    );
    spec.user = wire.user;
    spec.validate()
        .map_err(|error| invalid(error.to_string()))?;
    let mut config = HolderConfig::new(Persistence::Persistent);
    // The shim destroys this session on stop; retaining its exited state delays shutdown.
    config.retain_exited = Duration::ZERO;
    let handle = spawn(
        UnixSpawner,
        spec,
        config,
        LogContext {
            workload_id: workload.id.label(),
            session_id: id.label(),
        },
        GhosttyEmulator::new,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let socket = TerminalSocket::bind(
        root,
        &workload.id.label(),
        &id.label(),
        process::geteuid().as_raw(),
    )?;
    let path = socket.path().to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let _ = socket.serve_until(&handle, &flag);
        handle.send(Command::Close);
        handle.join();
    });
    Ok((id, HostedSession { path, stop, thread }))
}

fn label_hash(id: &StableId) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(id.label().as_bytes());
    let mut result = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(result, "{byte:02x}").expect("string write");
    }
    result
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
