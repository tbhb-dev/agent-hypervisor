//! Local hook socket transport. The bound listener, not a payload claim, identifies a session.

use std::fs::{self, Permissions};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hypervisor_core::state::{Harness, HookFields, HookKind, normalize_hook_fields};
use hypervisor_session::{Command, SessionHandle};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Maximum byte length of a socket pathname on the capture host is 103.
const MAX_SOCKET_PATH: usize = 103;
const MAX_PAYLOAD: u64 = 64 * 1024;

/// A state event delivered to the listener named by `path`.
#[derive(Debug, PartialEq, Eq)]
pub struct ReceivedHook {
    pub harness: Harness,
    pub kind: HookKind,
    pub seq: u64,
    /// The claimed harness session ID is audit data, never routing authority.
    pub claimed_session_id: Option<String>,
    /// Peer identity is audit data; it does not name the byte's originating process.
    pub peer_uid: u32,
    pub peer_gid: u32,
}

/// One session's private listener.
pub struct HookSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl HookSocket {
    /// Bind under a short runtime root, with a private 0700 workspace directory.
    ///
    /// # Errors
    /// If the path is too long, a socket already exists, or binding fails.
    pub fn bind(root: &Path, workspace_id: &str, session_id: &str) -> io::Result<Self> {
        let workspace = short_hash(workspace_id);
        let session = short_hash(session_id);
        let dir = root.join("r").join(workspace);
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        let path = dir.join(format!("{session}.s"));
        if path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hook socket path exceeds 103 bytes",
            ));
        }
        let listener = UnixListener::bind(&path)?;
        Ok(Self { listener, path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Receive a single trimmed event. Callers forward the result to this socket's holder.
    ///
    /// # Errors
    /// If accept, peer audit, or parsing fails.
    pub fn receive_one(&mut self) -> io::Result<ReceivedHook> {
        let (mut stream, _) = self.listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_millis(150)))?;
        let (peer_uid, peer_group) = peer_ids(&stream)?;
        let mut bytes = Vec::new();
        io::BufReader::new(Read::by_ref(&mut stream).take(MAX_PAYLOAD + 1))
            .read_until(b'\n', &mut bytes)?;
        if bytes.len() as u64 > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "hook payload too large",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        let harness = match value.get("harness").and_then(Value::as_str) {
            Some("claude") => Harness::Claude,
            Some("codex") => Harness::Codex,
            Some("agy") => Harness::Agy,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown harness",
                ));
            }
        };
        let hook = normalize_hook_fields(harness, decoded_fields(&value));
        if hook.name.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "missing event"));
        }
        let seq = value
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing source seq"))?;
        let _ = stream.write_all(b"\n");
        Ok(ReceivedHook {
            harness,
            kind: hook.kind,
            seq,
            claimed_session_id: value
                .get("claimed_session_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            peer_uid,
            peer_gid: peer_group,
        })
    }

    /// Receive one event and send it to the holder named by this listener.
    ///
    /// # Errors
    /// If the socket or payload is invalid.
    pub fn receive_into(&mut self, session: &SessionHandle) -> io::Result<ReceivedHook> {
        let report = self.receive_one()?;
        session.send(Command::Hook {
            harness: report.harness,
            kind: report.kind,
            seq: report.seq,
        });
        Ok(report)
    }
}

impl Drop for HookSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn short_hash(value: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(value.as_bytes());
    let mut hash = String::with_capacity(12);
    for byte in &digest[..6] {
        write!(hash, "{byte:02x}").expect("writing into a String cannot fail");
    }
    hash
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "getpeereid is the macOS peer credential API")]
fn peer_ids(stream: &UnixStream) -> io::Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: getpeereid writes the two initialized ids; the stream owns a valid socket fd.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if result == 0 {
        Ok((uid, gid))
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code, reason = "SO_PEERCRED requires getsockopt")]
fn peer_ids(stream: &UnixStream) -> io::Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .expect("ucred size fits socklen_t");
    // SAFETY: getsockopt writes at most len bytes into the initialized ucred.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    if result == 0 {
        Ok((cred.uid, cred.gid))
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Keep only the fields that state mapping needs from a raw hook payload.
#[must_use]
pub fn trimmed_event(harness: Harness, value: &Value) -> Value {
    let hook = normalize_hook_fields(harness, decoded_fields(value));
    let mut event = serde_json::json!({
        "harness": match harness { Harness::Claude => "claude", Harness::Codex => "codex", Harness::Agy => "agy" },
        "event": hook.name,
    });
    if let Some(kind) = hook.notification_type {
        event["notification_type"] = Value::String(kind.to_owned());
    }
    if let Some(id) = hook.claimed_session_id {
        event["claimed_session_id"] = Value::String(id.to_owned());
    }
    if let Some(idle) = hook.fully_idle {
        event["fully_idle"] = Value::Bool(idle);
    }
    event
}

fn decoded_fields(value: &Value) -> HookFields<'_> {
    HookFields {
        hook_event_name: value.get("hook_event_name").and_then(Value::as_str),
        event: value.get("event").and_then(Value::as_str),
        notification_type: value.get("notification_type").and_then(Value::as_str),
        enums_notification_type: value
            .get("enums")
            .and_then(|enums| enums.get("notification_type"))
            .and_then(Value::as_str),
        session_id: value.get("session_id").and_then(Value::as_str),
        conversation_id: value.get("conversationId").and_then(Value::as_str),
        fully_idle: value.get("fullyIdle").and_then(Value::as_bool),
        fully_idle_snake: value.get("fully_idle").and_then(Value::as_bool),
        enums_fully_idle: value
            .get("enums")
            .and_then(|enums| enums.get("fullyIdle"))
            .and_then(Value::as_bool),
    }
}
