//! Local terminal channel transport for one already resolved session.

use std::fs::{self, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use hypervisor_core::channel::{Encoding, Frame, MAX_FRAME, OpenRefusal, OpenRefused, VERSION};
use hypervisor_core::session::ViewerId;
use hypervisor_core::terminal_transport::{self, AfterEvent};
use hypervisor_session::SessionHandle;
use hypervisor_session::channel::Channel;

use crate::{MAX_SOCKET_PATH, peer_ids, short_hash};

const TICK: Duration = Duration::from_millis(10);

/// A listener whose path selects one session. `allowed_uid` is the local principal
/// allowed to connect; peer IDs are connection audit fields, not byte attribution.
pub struct TerminalSocket {
    listener: UnixListener,
    path: PathBuf,
    allowed_uid: u32,
    session_id: String,
}

impl TerminalSocket {
    /// Bind a per-session socket in a private workspace directory.
    ///
    /// # Errors
    /// If the path is too long, already bound, or inaccessible.
    pub fn bind(
        root: &Path,
        workspace_id: &str,
        session_id: &str,
        allowed_uid: u32,
    ) -> io::Result<Self> {
        let dir = root.join("r").join(short_hash(workspace_id));
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        let path = dir.join(format!("{}.c", short_hash(session_id)));
        if path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal socket path exceeds 103 bytes",
            ));
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            allowed_uid,
            session_id: session_id.into(),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serve concurrent local viewers until `stop` is set. The caller owns the
    /// session lifetime and supplies the local principal permitted at this path.
    ///
    /// # Errors
    /// If accepting a connection fails.
    pub fn serve_until(&self, session: &SessionHandle, stop: &AtomicBool) -> io::Result<()> {
        let start = Instant::now();
        let mut clients = Vec::new();
        let mut next_viewer = 1_u64;
        while !stop.load(Ordering::Relaxed) {
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        let (uid, gid) = match peer_ids(&stream) {
                            Ok(ids) => ids,
                            Err(error) => {
                                eprintln!("terminal peer credential lookup failed: {error}");
                                continue;
                            }
                        };
                        eprintln!(
                            "terminal peer session={} viewer={} uid={} gid={} accepted={}",
                            self.session_id,
                            next_viewer,
                            uid,
                            gid,
                            terminal_transport::admit_peer(uid, self.allowed_uid)
                        );
                        if !terminal_transport::admit_peer(uid, self.allowed_uid) {
                            continue;
                        }
                        stream.set_nonblocking(true)?;
                        clients.push(Client::new(stream, ViewerId(next_viewer)));
                        next_viewer = next_viewer
                            .checked_add(1)
                            .ok_or_else(|| io::Error::other("viewer ids exhausted"))?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
            let mut index = 0;
            while index < clients.len() {
                if clients[index].step(session, &self.session_id, start.elapsed()) {
                    index += 1;
                } else {
                    clients.swap_remove(index);
                }
            }
            thread::sleep(TICK);
        }
        Ok(())
    }
}

impl Drop for TerminalSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct Client<'a> {
    stream: UnixStream,
    viewer: ViewerId,
    channel: Option<Channel<'a>>,
    encoding: Option<Encoding>,
    incoming: Vec<u8>,
    outgoing: Vec<u8>,
    written: usize,
    close_after_flush: bool,
}

impl<'a> Client<'a> {
    fn new(stream: UnixStream, viewer: ViewerId) -> Self {
        Self {
            stream,
            viewer,
            channel: None,
            encoding: None,
            incoming: Vec::new(),
            outgoing: Vec::new(),
            written: 0,
            close_after_flush: false,
        }
    }

    // A false return closes the stream and drops the channel, detaching its viewer.
    fn step(&mut self, session: &'a SessionHandle, session_id: &str, now: Duration) -> bool {
        let mut buf = [0; 16 * 1024];
        match self.stream.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => self.incoming.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
        if self.incoming.len() > MAX_FRAME + 4 {
            return false;
        }
        loop {
            let Ok(decoded) = Frame::decode(&self.incoming) else {
                return false;
            };
            let Some((frame, consumed)) = decoded else {
                break;
            };
            self.incoming.drain(..consumed);
            if !self.receive(session, session_id, frame) {
                return false;
            }
            if self.close_after_flush {
                break;
            }
        }
        if let Some(channel) = self.channel.as_mut()
            && !self.close_after_flush
        {
            if let Some(frame) = channel.next_event() {
                let action = match &frame {
                    Frame::Event(event) => terminal_transport::after_event(
                        event,
                        self.encoding.expect("an open channel has an encoding"),
                    ),
                    _ => AfterEvent::Continue,
                };
                if !self.queue(&frame) {
                    return false;
                }
                if action == AfterEvent::ByteSnapshot {
                    let Some(channel) = self.channel.as_mut() else {
                        return false;
                    };
                    let Ok(snapshot) = channel.snapshot() else {
                        return false;
                    };
                    if !self.queue(&snapshot) {
                        return false;
                    }
                }
            }
            let Some(channel) = self.channel.as_mut() else {
                return false;
            };
            let output = if self.encoding == Some(Encoding::Grid) {
                channel.poll_grid(now)
            } else {
                channel.next_output()
            };
            match output {
                Ok(Some(frame)) => {
                    if !self.queue(&frame) {
                        return false;
                    }
                }
                Ok(None) => {}
                Err(_) => return false,
            }
        }
        while self.written < self.outgoing.len() {
            match self.stream.write(&self.outgoing[self.written..]) {
                Ok(0) => return false,
                Ok(n) => self.written += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        if self.written == self.outgoing.len() {
            self.outgoing.clear();
            self.written = 0;
            return !self.close_after_flush;
        }
        true
    }

    fn receive(&mut self, session: &'a SessionHandle, session_id: &str, frame: Frame) -> bool {
        if let Some(channel) = self.channel.as_mut() {
            let detach = matches!(
                frame,
                Frame::Control(hypervisor_core::channel::Control::Detach)
            );
            match channel.receive(frame) {
                Ok(Some(answer)) => {
                    self.close_after_flush = detach;
                    self.queue(&answer)
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else if let Frame::OpenRequest(request) = frame {
            if !terminal_transport::targets_session(&request.target, session_id) {
                self.close_after_flush = true;
                return self.queue(&refusal(OpenRefusal::UnknownTarget));
            }
            match Channel::open(session, self.viewer, &request) {
                Ok((channel, response)) => {
                    self.channel = Some(channel);
                    self.encoding = Some(request.encoding);
                    self.queue(&response)
                }
                Err(refused) => {
                    self.close_after_flush = true;
                    self.queue(&refused)
                }
            }
        } else {
            false
        }
    }

    fn queue(&mut self, frame: &Frame) -> bool {
        let Ok(bytes) = frame.encode() else {
            return false;
        };
        if self.written > 0 {
            self.outgoing.drain(..self.written);
            self.written = 0;
        }
        if !terminal_transport::queue_admits(self.outgoing.len(), bytes.len()) {
            return false;
        }
        self.outgoing.extend(bytes);
        true
    }
}

fn refusal(reason: OpenRefusal) -> Frame {
    Frame::OpenRefused(OpenRefused {
        reason,
        supported_versions: vec![VERSION],
    })
}
