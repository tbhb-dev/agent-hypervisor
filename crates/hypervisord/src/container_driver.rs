//! CLI-backed Apple container lifecycle and local terminal socket proxy.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hypervisor_core::channel::{WireSize, WireSpawnSpec};
use hypervisor_core::container::{self, GuestRequest};
use hypervisor_core::workload::{
    self, HostRequest, HostResponse, Recovery, StableId, WorkloadSpec,
};
use rustix::process;

use crate::short_hash;

const START_WAIT: Duration = Duration::from_secs(15);

struct Proxy {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    path: PathBuf,
}

/// One private registry. Dropping the driver leaves running workspace containers alive.
pub struct ContainerDriver {
    root: PathBuf,
    host: String,
    agent: PathBuf,
    workloads: BTreeMap<String, (WorkloadSpec, Recovery)>,
    proxies: BTreeMap<String, Proxy>,
}

impl ContainerDriver {
    /// Open saved workloads and adopt agents that answer at their published socket.
    ///
    /// # Errors
    /// Invalid registry, agent path, or filesystem I/O.
    pub fn open(root: &Path, host: &str, guest_agent: &Path) -> io::Result<Self> {
        StableId {
            host: host.into(),
            local: "probe".into(),
        }
        .validate()
        .map_err(invalid)?;
        fs::create_dir_all(root)?;
        fs::set_permissions(root, Permissions::from_mode(0o700))?;
        let root = fs::canonicalize(root)?;
        let agent = fs::canonicalize(guest_agent)?;
        if !agent.is_file() || root.as_os_str().len() > 65 {
            return Err(invalid(
                "guest agent must be a file and runtime root must be short",
            ));
        }
        let mut driver = Self {
            root,
            host: host.into(),
            agent,
            workloads: BTreeMap::new(),
            proxies: BTreeMap::new(),
        };
        for entry in fs::read_dir(&driver.root)? {
            let entry = entry?;
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            if !name.ends_with(".json") || name.ends_with(".guest.json") {
                continue;
            }
            let spec: WorkloadSpec = serde_json::from_reader(File::open(entry.path())?)?;
            container::validate(&spec).map_err(invalid)?;
            if spec.id.host != host || entry.path() != driver.metadata(&spec.id) {
                return Err(invalid("foreign or misnamed container workload"));
            }
            driver
                .workloads
                .insert(spec.id.label(), (spec, Recovery::Stopped));
        }
        let ids: Vec<_> = driver
            .workloads
            .values()
            .map(|(spec, _)| spec.id.clone())
            .collect();
        for id in ids {
            if matches!(driver.call(&id, &HostRequest::Ping), Ok(HostResponse::Identity(found)) if found == id)
            {
                if let Some(saved) = driver.workloads.get_mut(&id.label()) {
                    saved.1 = Recovery::Running;
                }
                if let Ok(HostResponse::Sessions(sessions)) = driver.call(&id, &HostRequest::List) {
                    for session in sessions {
                        let guest_path = guest_session_path(&id, &session);
                        driver.proxy_session(&id, &session, &guest_path)?;
                    }
                }
            }
        }
        Ok(driver)
    }

    /// Register a canonical workspace and its private named cache volume.
    ///
    /// # Errors
    /// Invalid request or failed filesystem or container CLI operation.
    pub fn create(&mut self, mut spec: WorkloadSpec) -> io::Result<()> {
        container::validate(&spec).map_err(invalid)?;
        if spec.id.host != self.host || self.workloads.contains_key(&spec.id.label()) {
            return Err(invalid("foreign or duplicate container workload"));
        }
        spec.workspace_dir = fs::canonicalize(&spec.workspace_dir)?;
        spec.cache_dir = fs::canonicalize(&spec.cache_dir)?;
        for mount in &mut spec.mounts {
            mount.source = fs::canonicalize(&mount.source)?;
        }
        container::validate(&spec).map_err(invalid)?;
        validate_socket_mounts(&spec)?;
        let host_path = self.metadata(&spec.id);
        let guest_path = self.guest_metadata(&spec.id);
        let mut host_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&host_path)?;
        if let Err(error) = serde_json::to_writer(&mut host_file, &spec) {
            let _ = fs::remove_file(&host_path);
            return Err(error.into());
        }
        if let Err(error) = host_file.flush() {
            let _ = fs::remove_file(&host_path);
            return Err(error);
        }
        let result = (|| -> io::Result<()> {
            let mut guest_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&guest_path)?;
            serde_json::to_writer(&mut guest_file, &container::guest_spec(&spec))?;
            guest_file.flush()?;
            cli(&[
                "volume".into(),
                "create".into(),
                container::cache_volume(&spec.id),
            ])?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(host_path);
            let _ = fs::remove_file(guest_path);
            return Err(error);
        }
        self.workloads
            .insert(spec.id.label(), (spec, Recovery::Stopped));
        Ok(())
    }

    /// Boot one long-lived guest agent and prove its workload identity.
    ///
    /// # Errors
    /// Unknown workload, CLI failure, or failed guest handshake.
    pub fn start(&mut self, id: &StableId) -> io::Result<()> {
        let (spec, recovery) = self
            .workloads
            .get(&id.label())
            .ok_or_else(|| invalid("unknown workload"))?;
        workload::admit_start(*recovery, self.call(id, &HostRequest::Ping).is_ok())
            .map_err(invalid)?;
        let args = container::run(
            spec,
            &self.agent,
            &self.guest_metadata(id),
            &self.socket(id),
        )
        .map_err(invalid)?
        .0;
        validate_socket_mounts(spec)?;
        // Replace an orphaned stopped container before starting a new one with the same name.
        if cli(&["inspect".into(), container::name(id)]).is_ok() {
            let _ = cli(&[
                "stop".into(),
                "--time".into(),
                "0".into(),
                container::name(id),
            ]);
            cli(&["delete".into(), container::name(id)])?;
        }
        if let Err(error) = cli(&args) {
            if cli(&["inspect".into(), container::name(id)]).is_ok() {
                self.remove_container(id)?;
            }
            return Err(error);
        }
        let started = Instant::now();
        while started.elapsed() < START_WAIT {
            if matches!(self.call(id, &HostRequest::Ping), Ok(HostResponse::Identity(found)) if found == *id)
            {
                self.workloads
                    .get_mut(&id.label())
                    .ok_or_else(|| invalid("unknown workload"))?
                    .1 = Recovery::Running;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = self.remove_container(id);
        Err(io::Error::other(
            "guest agent did not complete its identity handshake",
        ))
    }

    /// Stop and remove the VM without waiting on guest cooperation. The cache volume survives.
    ///
    /// # Errors
    /// Unknown workload or CLI failure.
    pub fn stop(&mut self, id: &StableId) -> io::Result<()> {
        if !self.workloads.contains_key(&id.label()) {
            return Err(invalid("unknown workload"));
        }
        self.remove_container(id)?;
        self.drop_proxies(id);
        self.workloads
            .get_mut(&id.label())
            .ok_or_else(|| invalid("unknown workload"))?
            .1 = Recovery::Stopped;
        Ok(())
    }

    /// Remove the stopped workload, its private cache volume, and metadata.
    ///
    /// # Errors
    /// Running workload or failed cleanup.
    pub fn destroy(&mut self, id: &StableId) -> io::Result<()> {
        let (_, recovery) = self
            .workloads
            .get(&id.label())
            .ok_or_else(|| invalid("unknown workload"))?;
        if *recovery != Recovery::Stopped {
            return Err(invalid("stop the container before destroy"));
        }
        self.drop_proxies(id);
        if cli(&["inspect".into(), container::name(id)]).is_ok() {
            self.remove_container(id)?;
        }
        cli(&[
            "volume".into(),
            "delete".into(),
            container::cache_volume(id),
        ])?;
        fs::remove_file(self.metadata(id))?;
        fs::remove_file(self.guest_metadata(id))?;
        self.workloads.remove(&id.label());
        Ok(())
    }

    /// Start a guest-held PTY session and publish a local terminal channel for it.
    ///
    /// # Errors
    /// Invalid guest user, agent refusal, or failed socket proxy.
    pub fn spawn_session(
        &mut self,
        workload_id: &StableId,
        id: StableId,
        spec: WireSpawnSpec,
        size: WireSize,
    ) -> io::Result<PathBuf> {
        if id.host != workload_id.host {
            return Err(invalid("foreign session"));
        }
        container::session_user(&spec).map_err(invalid)?;
        let session_id = id.clone();
        let guest_path = match self.call(workload_id, &HostRequest::Spawn { id, spec, size })? {
            HostResponse::SessionSocket(path) => path,
            HostResponse::Error(error) => return Err(io::Error::other(error)),
            _ => return Err(io::Error::other("unexpected guest response")),
        };
        self.proxy_session(workload_id, &session_id, &guest_path)
    }

    /// Query the guest holder, not the `container exec` clients.
    ///
    /// # Errors
    /// The guest socket is unavailable or returns an invalid response.
    pub fn list_sessions(&self, id: &StableId) -> io::Result<Vec<StableId>> {
        match self.call(id, &HostRequest::List)? {
            HostResponse::Sessions(ids) => Ok(ids),
            _ => Err(io::Error::other("unexpected guest response")),
        }
    }

    /// Return the guest holder's live session count.
    ///
    /// # Errors
    /// The guest socket is unavailable or returns an invalid response.
    pub fn stats(&self, id: &StableId) -> io::Result<usize> {
        match self.call(id, &HostRequest::Stats)? {
            HostResponse::Stats { sessions, .. } => Ok(sessions),
            _ => Err(io::Error::other("unexpected guest response")),
        }
    }

    /// Return the guest holder's in-memory lifecycle events.
    ///
    /// # Errors
    /// The guest socket is unavailable or returns an invalid response.
    pub fn events(&self, id: &StableId) -> io::Result<Vec<String>> {
        match self.call(id, &HostRequest::Events)? {
            HostResponse::Events(events) => Ok(events),
            _ => Err(io::Error::other("unexpected guest response")),
        }
    }

    #[must_use]
    pub fn recovery(&self, id: &StableId) -> Option<Recovery> {
        self.workloads
            .get(&id.label())
            .map(|(_, recovery)| *recovery)
    }

    fn proxy_session(
        &mut self,
        workload: &StableId,
        session: &StableId,
        guest_path: &Path,
    ) -> io::Result<PathBuf> {
        let expected = guest_session_path(workload, session);
        if guest_path != expected {
            return Err(invalid("guest terminal socket path does not match session"));
        }
        let path = self.terminal_socket(workload, session);
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(invalid("terminal proxy already running"));
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let guest_socket = self.socket(workload);
        let target = guest_path.to_path_buf();
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((viewer, _)) => {
                        if crate::peer_ids(&viewer)
                            .is_ok_and(|(uid, _)| uid == process::geteuid().as_raw())
                        {
                            let guest_socket = guest_socket.clone();
                            let target = target.clone();
                            thread::spawn(move || {
                                let _ = relay(viewer, &guest_socket, &target);
                            });
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        self.proxies.insert(
            format!("{}/{}", workload.label(), session.label()),
            Proxy {
                stop,
                thread,
                path: path.clone(),
            },
        );
        Ok(path)
    }

    fn drop_proxies(&mut self, id: &StableId) {
        let sessions: Vec<_> = self
            .proxies
            .keys()
            .filter(|key| key.starts_with(&format!("{}/", id.label())))
            .cloned()
            .collect();
        for session in sessions {
            if let Some(proxy) = self.proxies.remove(&session) {
                proxy.stop.store(true, Ordering::Relaxed);
                let _ = proxy.thread.join();
                let _ = fs::remove_file(proxy.path);
            }
        }
    }

    fn remove_container(&self, id: &StableId) -> io::Result<()> {
        let name = container::name(id);
        if cli(&["stop".into(), "--time".into(), "0".into(), name.clone()]).is_err() {
            cli(&["delete".into(), "--force".into(), name])?;
        } else {
            cli(&["delete".into(), name])?;
        }
        let socket = self.socket(id);
        if socket.exists() {
            fs::remove_file(socket)?;
        }
        Ok(())
    }

    fn metadata(&self, id: &StableId) -> PathBuf {
        self.root.join(format!("{}.json", container::name(id)))
    }
    fn guest_metadata(&self, id: &StableId) -> PathBuf {
        self.root
            .join(format!("{}.guest.json", container::name(id)))
    }
    fn socket(&self, id: &StableId) -> PathBuf {
        self.root.join(format!("{}.sock", short_hash(&id.label())))
    }
    fn terminal_socket(&self, workload: &StableId, session: &StableId) -> PathBuf {
        self.root.join(format!(
            "{}-{}.c",
            short_hash(&workload.label()),
            short_hash(&session.label())
        ))
    }

    fn call(&self, id: &StableId, request: &HostRequest) -> io::Result<HostResponse> {
        if !self.workloads.contains_key(&id.label()) {
            return Err(invalid("unknown workload"));
        }
        let mut stream = UnixStream::connect(self.socket(id))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        serde_json::to_writer(&mut stream, &GuestRequest::Control(request.clone()))?;
        stream.write_all(b"\n")?;
        stream.shutdown(std::net::Shutdown::Write)?;
        serde_json::from_reader(stream).map_err(Into::into)
    }
}

impl Drop for ContainerDriver {
    fn drop(&mut self) {
        for (_, proxy) in std::mem::take(&mut self.proxies) {
            proxy.stop.store(true, Ordering::Relaxed);
            let _ = proxy.thread.join();
            let _ = fs::remove_file(proxy.path);
        }
    }
}

fn guest_session_path(workload: &StableId, session: &StableId) -> PathBuf {
    Path::new(container::GUEST_ROOT)
        .join("r")
        .join(short_hash(&workload.label()))
        .join(format!("{}.c", short_hash(&session.label())))
}

fn validate_socket_mounts(spec: &WorkloadSpec) -> io::Result<()> {
    for mount in &spec.mounts {
        let info = fs::metadata(&mount.source)?;
        let broker = mount.target == Path::new("/run/broker.sock");
        if broker != info.file_type().is_socket() {
            return Err(invalid(
                "only the broker target accepts a Unix socket mount",
            ));
        }
        if broker
            && (info.permissions().mode() & 0o777 != 0o600
                || mount.mode != hypervisor_core::workload::MountMode::ReadOnly)
        {
            return Err(invalid(
                "broker socket must be mode 0600 and read-only in the workload spec",
            ));
        }
    }
    Ok(())
}

fn relay(mut viewer: UnixStream, guest_socket: &Path, target: &Path) -> io::Result<()> {
    let mut guest = UnixStream::connect(guest_socket)?;
    serde_json::to_writer(&mut guest, &GuestRequest::Attach(target.to_path_buf()))?;
    guest.write_all(b"\n")?;
    let mut viewer_read = viewer.try_clone()?;
    let mut guest_write = guest.try_clone()?;
    thread::scope(|scope| {
        scope.spawn(move || {
            let _ = io::copy(&mut viewer_read, &mut guest_write);
            let _ = guest_write.shutdown(std::net::Shutdown::Write);
        });
        let _ = io::copy(&mut guest, &mut viewer);
        let _ = viewer.shutdown(std::net::Shutdown::Read);
    });
    Ok(())
}

fn cli(args: &[String]) -> io::Result<()> {
    let output = Command::new("container")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "container {} failed with {}",
        args.first().map_or("?", String::as_str),
        output.status
    )))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
