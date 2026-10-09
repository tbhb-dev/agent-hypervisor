//! Host daemon shell and fail-open hook client.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hypervisor_core::state::Harness;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if mode.as_deref() == Some("adopt") {
        let args: Vec<String> = args.collect();
        if let Err(error) = adopt(&args) {
            eprintln!("hypervisord adopt: {error}");
            std::process::exit(1);
        }
        return;
    }
    if mode.as_deref() == Some("daemon") {
        let args: Vec<String> = args.collect();
        let [root, host, shim, log, socket] = args.as_slice() else {
            eprintln!("usage: hypervisord daemon ROOT HOST SHIM LOG SOCKET");
            std::process::exit(2);
        };
        let result = hypervisord::control::run_daemon(
            root.as_ref(),
            host,
            shim.as_ref(),
            log.as_ref(),
            socket.as_ref(),
        );
        if let Err(error) = result {
            eprintln!("hypervisord daemon: {error}");
            std::process::exit(1);
        }
        return;
    }
    if mode.as_deref() != Some("hook") {
        println!(
            "{}",
            hypervisor_core::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
        );
        return;
    }
    let harness = match args.next().as_deref() {
        Some("claude") => Harness::Claude,
        Some("codex") => Harness::Codex,
        Some("agy") => Harness::Agy,
        _ => return,
    };
    let path = args.next().map(PathBuf::from);
    // Capture source order before stdin or the socket can delay this invocation.
    let seq = source_sequence();
    let agy_event = if harness == Harness::Agy {
        args.next()
    } else {
        None
    };
    if agy_event.as_deref() == Some("PreToolUse") {
        // An empty response denies agy's PreToolUse, including when the listener is absent.
        println!("{{\"decision\":\"ask\"}}");
    }
    let Some(path) = path else {
        return;
    };
    let Some(seq) = seq else {
        return;
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > 64 * 1024
    {
        return;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&input) else {
        return;
    };
    if let Some(name) = agy_event {
        value["event"] = serde_json::Value::String(name);
    }
    let mut event = hypervisord::trimmed_event(harness, &value);
    event["seq"] = seq.into();
    let event = format!("{event}\n");
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        use std::io::Write;
        if let Ok(mut stream) = UnixStream::connect(path) {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            let _ = stream.write_all(event.as_bytes());
            let mut ack = [0];
            let _ = stream.read(&mut ack);
        }
        let _ = done.send(());
    });
    let _ = finished.recv_timeout(Duration::from_millis(150));
}

/// Start as a daemon would: adopt launchd shims from `<root> <host> <shim> <label-prefix>`
/// and print each workload's recovery, session count, and shim PID.
fn adopt(args: &[String]) -> std::io::Result<()> {
    let [root, host, shim, prefix] = args else {
        return Err(std::io::Error::other("usage: adopt ROOT HOST SHIM PREFIX"));
    };
    let config = hypervisor_core::launchd::JobConfig {
        prefix: prefix.clone(),
        uid: rustix::process::geteuid().as_raw(),
        throttle_seconds: 10,
    };
    let driver =
        hypervisord::driver::HostDriver::open_launchd(root.as_ref(), host, shim.as_ref(), config)?;
    for (label, workload) in driver.workloads() {
        let id = &workload.spec.id;
        let (sessions, pid) = match workload.recovery {
            hypervisor_core::workload::Recovery::Running
            | hypervisor_core::workload::Recovery::Restarted => {
                (driver.stats(id)?, driver.shim_pid(id)?)
            }
            _ => (0, 0),
        };
        println!("{label} {:?} {sessions} {pid}", workload.recovery);
    }
    Ok(())
}

#[allow(unsafe_code, reason = "clock_gettime reads the host monotonic clock")]
fn source_sequence() -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: now is initialized and points to writable timespec storage.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return None;
    }
    u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)
}
