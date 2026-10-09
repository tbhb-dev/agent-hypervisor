//! The Unix backend: `posix_openpt`, `setsid`, and `TIOCSCTTY`, through `rustix`.
//!
//! Observed on macOS; CI runs the same tests on Linux, which follows the same rules for every
//! call here (RFC-40 run 11's per-OS table).

use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};

use hypervisor_core::emulator::Size;
use hypervisor_core::session::{Exit, RESTORED_SIGNALS, RestoredSignal, Signal, SpawnSpec, Target};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
#[cfg(not(target_os = "linux"))]
use rustix::io::FdFlags;
use rustix::process::{self as rp, Pid};
use rustix::pty::{self, OpenptFlags};
use rustix::termios::{self, Winsize};

use crate::{Caps, Pty, PtyError, Resize, Spawn, Support, Wait};

/// Spawns children on new Unix PTYs as the current user.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnixSpawner;

/// Linux guest PTY spawner. Reviewer children get a fail-closed read-only Landlock policy.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
pub struct GuestSpawner {
    pub uid: u32,
    pub reviewer: bool,
}

/// A Unix PTY's master side and its child's process ID, which is also its process group and
/// session ID.
#[derive(Debug)]
pub struct UnixPty {
    master: File,
    pid: Pid,
    waiter: Option<UnixWaiter>,
}

/// Reads a Unix PTY's output. `EIO`, which the master returns once every slave handle has
/// closed, reads as the end of output.
#[derive(Debug)]
pub struct UnixReader(File);

/// Owns the child process until it exits.
#[derive(Debug)]
pub struct UnixWaiter(Child);

fn winsize(size: Size) -> Winsize {
    Winsize {
        ws_row: size.rows(),
        ws_col: size.cols(),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

const fn signal_number(signal: Signal) -> rp::Signal {
    match signal {
        Signal::Hangup => rp::Signal::HUP,
        Signal::Interrupt => rp::Signal::INT,
        Signal::Terminate => rp::Signal::TERM,
        Signal::Kill => rp::Signal::KILL,
    }
}

/// Held across every spawn. macOS `posix_openpt` can't set close-on-exec atomically, so a fork
/// from another session's spawn between `openpt` and `FD_CLOEXEC` would leave that child holding
/// this master, and dropping ours would never hang up the slave. The lock keeps this backend's
/// own forks out of that window; forks from outside the backend are not covered.
static SPAWN: Mutex<()> = Mutex::new(());

#[expect(
    unsafe_code,
    reason = "restoring signal dispositions requires sigaction"
)]
fn restore_signal_dispositions() -> io::Result<()> {
    // SAFETY: SIG_DFL is a valid disposition and sigaction is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        for signal in RESTORED_SIGNALS.map(|signal| match signal {
            RestoredSignal::Hangup => libc::SIGHUP,
            RestoredSignal::Interrupt => libc::SIGINT,
            RestoredSignal::Quit => libc::SIGQUIT,
            RestoredSignal::Terminate => libc::SIGTERM,
            RestoredSignal::Pipe => libc::SIGPIPE,
        }) {
            if libc::sigaction(signal, &raw const action, std::ptr::null_mut()) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Opens a PTY master that is closed on exec.
fn open_master() -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let master = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    #[cfg(not(target_os = "linux"))]
    let master = {
        let master = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        rustix::io::fcntl_setfd(&master, FdFlags::CLOEXEC)?;
        master
    };
    Ok(master)
}

impl Spawn for UnixSpawner {
    type Pty = UnixPty;

    fn spawn(&self, spec: &SpawnSpec) -> Result<UnixPty, PtyError> {
        spawn_impl(spec, None, false)
    }
}

#[cfg(target_os = "linux")]
impl Spawn for GuestSpawner {
    type Pty = UnixPty;

    fn spawn(&self, spec: &SpawnSpec) -> Result<UnixPty, PtyError> {
        spawn_impl(spec, Some(self.uid), self.reviewer)
    }
}

fn spawn_impl(spec: &SpawnSpec, uid: Option<u32>, reviewer: bool) -> Result<UnixPty, PtyError> {
    let mut check = spec.clone();
    if uid.is_some() {
        check.user = None;
    }
    check.validate().map_err(PtyError::Spec)?;
    #[cfg(target_os = "linux")]
    let ruleset = if reviewer {
        Some(reviewer_ruleset()?)
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let _ = reviewer;
    let _spawning = SPAWN.lock().unwrap_or_else(PoisonError::into_inner);
    let master = open_master()?;
    pty::grantpt(&master).map_err(io::Error::from)?;
    pty::unlockpt(&master).map_err(io::Error::from)?;
    let name = pty::ptsname(&master, Vec::new()).map_err(io::Error::from)?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    // The size is in place before the program starts, so its first read of it is right.
    termios::tcsetwinsize(&slave, winsize(spec.size)).map_err(io::Error::from)?;

    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = uid;
    // SAFETY: the hook runs in the forked child before exec and makes only `setsid`,
    // `ioctl`, and `sigaction` calls, which are async-signal-safe, on fd 0, which `stdin`
    // set above.
    #[expect(
        unsafe_code,
        reason = "pre_exec sets the controlling terminal and restores signal defaults"
    )]
    unsafe {
        cmd.pre_exec(move || {
            rp::setsid()?;
            rp::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            // Ignored dispositions survive exec. A session must receive terminal and
            // lifecycle signals even if its supervisor ignored them.
            restore_signal_dispositions()?;
            #[cfg(target_os = "linux")]
            {
                if let Some(ruleset) = &ruleset {
                    apply_reviewer_ruleset(ruleset.as_raw_fd())?;
                }
                if let Some(uid) = uid {
                    drop_guest_identity(uid)?;
                }
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = Pid::from_child(&child);
    // `cmd` holds the parent's copies of the slave; dropping it leaves the child the only
    // holder, so the master reads end of output when the child's tree is gone.
    drop(cmd);
    Ok(UnixPty {
        master: File::from(master),
        pid,
        waiter: Some(UnixWaiter(child)),
    })
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

// Linux landlock.h: every filesystem write or mutation right through ABI 5.
#[cfg(target_os = "linux")]
const WRITE_RIGHTS: u64 = 0xfffe;

#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "Landlock syscalls have no libc wrapper")]
fn reviewer_ruleset() -> io::Result<OwnedFd> {
    let attr = RulesetAttr {
        handled_access_fs: WRITE_RIGHTS,
    };
    // SAFETY: the kernel reads a fixed repr(C) struct for the duration of the call.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            std::mem::size_of::<RulesetAttr>(),
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful create_ruleset returns an owned file descriptor.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    for path in hypervisor_core::container::REVIEWER_WRITE_PATHS {
        let parent = rustix::fs::open(*path, OFlags::PATH | OFlags::CLOEXEC, Mode::empty())?;
        let rule = PathBeneathAttr {
            allowed_access: WRITE_RIGHTS,
            parent_fd: parent.as_raw_fd(),
        };
        // SAFETY: both FDs and the repr(C) rule remain valid during the syscall.
        if unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                1,
                &raw const rule,
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ruleset)
}

#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "Landlock restriction is installed in the child before exec"
)]
fn apply_reviewer_ruleset(fd: i32) -> io::Result<()> {
    // SAFETY: prctl and the Landlock syscall only use scalar arguments here.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0
        || unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "the guest child drops root groups and IDs before exec"
)]
fn drop_guest_identity(uid: u32) -> io::Result<()> {
    // SAFETY: each call uses scalar IDs or a null pointer with zero groups.
    if unsafe { libc::setgroups(0, std::ptr::null()) } < 0
        || unsafe { libc::setgid(uid) } < 0
        || unsafe { libc::setuid(uid) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Wait for UnixWaiter {
    fn wait(mut self) -> io::Result<Exit> {
        let status = self.0.wait()?;
        Ok(Exit {
            code: status.code(),
            signal: status.signal(),
            raw: Some(status.into_raw().cast_unsigned()),
        })
    }
}

impl Read for UnixReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(e) if e.raw_os_error() == Some(Errno::IO.raw_os_error()) => Ok(0),
            other => other,
        }
    }
}

impl UnixPty {
    /// The child's process ID.
    #[must_use]
    pub fn pid(&self) -> i32 {
        self.pid.as_raw_pid()
    }

    fn foreground_pid(&self) -> Option<Pid> {
        termios::tcgetpgrp(&self.master).ok()
    }
}

impl Pty for UnixPty {
    type Reader = UnixReader;
    type Waiter = UnixWaiter;

    fn reader(&self) -> io::Result<UnixReader> {
        Ok(UnixReader(self.master.try_clone()?))
    }

    fn take_waiter(&mut self) -> Option<UnixWaiter> {
        self.waiter.take()
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.master.write_all(bytes)
    }

    fn resize(&mut self, size: Size) -> io::Result<Resize> {
        let wanted = winsize(size);
        let current = termios::tcgetwinsize(&self.master)?;
        if (current.ws_col, current.ws_row) == (wanted.ws_col, wanted.ws_row) {
            return Ok(Resize::Unchanged);
        }
        termios::tcsetwinsize(&self.master, wanted)?;
        Ok(Resize::Applied)
    }

    fn signal(&self, signal: Signal, target: Target) -> io::Result<Support> {
        let sig = signal_number(signal);
        match target {
            Target::Leader => rp::kill_process(self.pid, sig)?,
            // `setsid` made the child a group leader, so its group ID is its PID.
            Target::Group => rp::kill_process_group(self.pid, sig)?,
            Target::Foreground => {
                let Some(group) = self.foreground_pid() else {
                    return Ok(Support::Unsupported);
                };
                rp::kill_process_group(group, sig)?;
            }
        }
        Ok(Support::Ok)
    }

    fn redraw_hint(&self) -> io::Result<Support> {
        let Some(group) = self.foreground_pid() else {
            return Ok(Support::Unsupported);
        };
        rp::kill_process_group(group, rp::Signal::WINCH)?;
        Ok(Support::Ok)
    }

    fn foreground(&self) -> Option<i32> {
        self.foreground_pid().map(Pid::as_raw_pid)
    }

    fn caps(&self) -> Caps {
        Caps {
            foreground: true,
            signals: true,
            redraw_hint: true,
            backend_queries: &[],
            resize_repaints: false,
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/support/process_group.rs"]
mod process_group;

#[cfg(test)]
mod signal_tests {
    use super::process_group;
    use super::restore_signal_dispositions;
    use std::os::unix::process::CommandExt;

    #[test]
    #[expect(unsafe_code, reason = "the test queries its own SIGPIPE disposition")]
    fn restores_ignored_sigpipe() {
        const TEST: &str = "unix::signal_tests::restores_ignored_sigpipe";
        if std::env::var_os("HYPERVISOR_TEST_RESTORE_SIGPIPE").is_some() {
            // Rust's test process ignores SIGPIPE, so exercise the helper directly here.
            // SAFETY: a null new action only queries this process's disposition.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &raw mut action) },
                0
            );
            assert_eq!(action.sa_sigaction, libc::SIG_IGN);
            restore_signal_dispositions().unwrap();
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &raw mut action) },
                0
            );
            assert_eq!(action.sa_sigaction, libc::SIG_DFL);
            return;
        }

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST])
            .env("HYPERVISOR_TEST_RESTORE_SIGPIPE", "1")
            .process_group(0)
            .spawn()
            .unwrap();
        let _group = process_group::ProcessGroup::new(child.id().cast_signed());
        assert!(child.wait().unwrap().success());
    }
}
