//! The daemon's control socket and the proxy process that forwards client calls to it.
//! Decisions come from `hypervisor_core::control`; this module only moves bytes and runs them.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use hypervisor_core::control::{
    self, Change, ClientRequest, ControlError, CorruptLine, EventLog, Forwarded, Operator, Plan,
    Request, Response,
};
use hypervisor_core::terminal_transport::admit_peer;
use sha2::{Digest, Sha256};

use crate::driver::HostDriver;
use crate::peer_ids;

const MAX_REQUEST: u64 = 1024 * 1024;
const FOLLOW_TICK: Duration = Duration::from_millis(100);

struct Daemon {
    driver: HostDriver,
    log: EventLog,
    file: File,
}

type Shared = (Mutex<Daemon>, Condvar);

/// Run the daemon: adopt shims under `root`, restore the host log, and serve `socket`.
///
/// # Errors
/// Driver, log, or socket setup failure.
pub fn run_daemon(
    root: &Path,
    host: &str,
    shim: &Path,
    log_path: &Path,
    socket: &Path,
) -> io::Result<()> {
    let driver = HostDriver::open(root, host, shim)?;
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(log_path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let (log, valid) = EventLog::restore(host, &bytes).map_err(|CorruptLine(line)| {
        io::Error::other(format!("event log line {line} is corrupt"))
    })?;
    // Drop a torn final append so the next one starts on a line boundary.
    file.set_len(valid as u64)?;
    let shared: Arc<Shared> = Arc::new((Mutex::new(Daemon { driver, log, file }), Condvar::new()));
    let me = rustix::process::geteuid().as_raw();
    serve(socket, move |stream| {
        let (uid, _) = peer_ids(&stream)?;
        // Only the proxy, running as the daemon's own principal here, may call the daemon.
        if admit_peer(uid, me) {
            handle(stream, &shared)
        } else {
            Ok(())
        }
    })
}

/// Run the proxy: accept clients on `socket` and forward each request to the daemon with the
/// client's peer credentials attached. The proxy holds no driver state and no log.
///
/// # Errors
/// Socket setup failure.
pub fn run_proxy(daemon: &Path, socket: &Path) -> io::Result<()> {
    let daemon = daemon.to_path_buf();
    serve(socket, move |mut client| {
        let (uid, gid) = peer_ids(&client)?;
        let request: ClientRequest = match read_line(&client).and_then(|line| {
            serde_json::from_str(&line).map_err(|error| io::Error::other(error.to_string()))
        }) {
            Ok(request) => request,
            Err(error) => {
                let error = ControlError::Failed(format!("invalid request: {error}"));
                return write_line(&mut client, &Response::Error(error));
            }
        };
        let mut upstream = UnixStream::connect(&daemon)?;
        write_line(
            &mut upstream,
            &control::forward(request, Operator { uid, gid }),
        )?;
        io::copy(&mut upstream, &mut client).map(drop)
    })
}

fn serve(
    socket: &Path,
    handler: impl Fn(UnixStream) -> io::Result<()> + Send + Sync + 'static,
) -> io::Result<()> {
    // A socket left by a killed process is removed only when nothing answers on it.
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(io::Error::other("control socket already in use"));
        }
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, Permissions::from_mode(0o600))?;
    let handler = Arc::new(handler);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("control accept failed: {error}");
                continue;
            }
        };
        let handler = Arc::clone(&handler);
        thread::spawn(move || {
            if let Err(error) = handler(stream) {
                eprintln!("control connection failed: {error}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: UnixStream, (lock, changed): &Shared) -> io::Result<()> {
    let forwarded: Forwarded = match serde_json::from_str(&read_line(&stream)?) {
        Ok(forwarded) => forwarded,
        Err(error) => {
            let error = ControlError::Failed(format!("invalid request: {error}"));
            return write_line(&mut stream, &Response::Error(error));
        }
    };
    let fingerprint = hex(&Sha256::digest(serde_json::to_vec(&forwarded.request)?));
    let mut daemon = locked(lock)?;
    let (events, mut cursor) = match control::plan(&daemon.log, &forwarded, &fingerprint) {
        Plan::Reply(response) => {
            drop(daemon);
            return write_line(&mut stream, &response);
        }
        Plan::Query => {
            let response = daemon.query(&forwarded.request);
            drop(daemon);
            return write_line(&mut stream, &response);
        }
        Plan::Execute { request_id } => {
            let response = daemon.execute(&forwarded, &request_id, &fingerprint);
            drop(daemon);
            changed.notify_all();
            return write_line(&mut stream, &response);
        }
        Plan::Stream(events) => (events.to_vec(), daemon.log.head()),
    };
    drop(daemon);
    for event in events {
        write_line(&mut stream, &Response::Event(event))?;
    }
    stream.set_nonblocking(true)?;
    loop {
        let daemon = locked(lock)?;
        let (daemon, _) = changed
            .wait_timeout_while(daemon, FOLLOW_TICK, |daemon| daemon.log.head() == cursor)
            .map_err(|_| io::Error::other("daemon state poisoned"))?;
        let events = daemon.log.after(Some(&cursor)).unwrap_or_default().to_vec();
        cursor = daemon.log.head();
        drop(daemon);
        stream.set_nonblocking(false)?;
        for event in events {
            write_line(&mut stream, &Response::Event(event))?;
        }
        stream.set_nonblocking(true)?;
        // The proxy sends nothing after its request, so a zero-length read means it left.
        match stream.read(&mut [0]) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            _ => return Ok(()),
        }
    }
}

impl Daemon {
    fn query(&self, request: &Request) -> Response {
        let result = match request {
            Request::ListWorkloads => Ok(Response::Workloads(
                self.driver
                    .workloads()
                    .values()
                    .map(|workload| workload.spec.id.clone())
                    .collect(),
            )),
            Request::ListSessions { workload } => {
                self.driver.list_sessions(workload).map(Response::Sessions)
            }
            Request::SessionDetail { workload, session } => {
                self.driver.list_sessions(workload).map(|live| {
                    self.log.session_detail(workload, session, &live).map_or(
                        Response::Error(ControlError::UnknownSession),
                        Response::Detail,
                    )
                })
            }
            _ => Err(io::Error::other("not a query")),
        };
        result.unwrap_or_else(|error| Response::Error(ControlError::Failed(error.to_string())))
    }

    fn execute(&mut self, forwarded: &Forwarded, request_id: &str, fingerprint: &str) -> Response {
        let change = match &forwarded.request {
            Request::SpawnSession {
                workload,
                session,
                spec,
                size,
            } => self
                .driver
                .spawn_session(workload, session.clone(), spec.clone(), *size)
                .map(|socket| Change::SessionSpawned {
                    workload: workload.clone(),
                    session: session.clone(),
                    socket,
                }),
            Request::KillSession { workload, session } => self
                .driver
                .kill_session(workload, session.clone())
                .map(|()| Change::SessionKilled {
                    workload: workload.clone(),
                    session: session.clone(),
                }),
            _ => Err(io::Error::other("not a mutation")),
        };
        let change = match change {
            Ok(change) => change,
            Err(error) => return Response::Error(ControlError::Failed(error.to_string())),
        };
        let (event, line) = self
            .log
            .append(request_id, fingerprint, forwarded.operator, change);
        if let Err(error) = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
        {
            // The in-memory log is ahead of the file; stop so a restart rebuilds from disk.
            eprintln!("event log append failed: {error}");
            std::process::exit(1);
        }
        control::outcome(&event)
    }
}

fn locked(lock: &Mutex<Daemon>) -> io::Result<MutexGuard<'_, Daemon>> {
    lock.lock()
        .map_err(|_| io::Error::other("daemon state poisoned"))
}

fn read_line(stream: &UnixStream) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST)).read_line(&mut line)?;
    Ok(line)
}

fn write_line(stream: &mut UnixStream, value: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        write!(text, "{byte:02x}").expect("writing into a String cannot fail");
        text
    })
}
