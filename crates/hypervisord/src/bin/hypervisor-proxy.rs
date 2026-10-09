//! The unprivileged proxy: `hypervisor-proxy DAEMON_SOCKET LISTEN_SOCKET`.

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [daemon, socket] = args.as_slice() else {
        eprintln!("usage: hypervisor-proxy DAEMON_SOCKET LISTEN_SOCKET");
        std::process::exit(2);
    };
    if let Err(error) = hypervisord::control::run_proxy(daemon.as_ref(), socket.as_ref()) {
        eprintln!("hypervisor-proxy: {error}");
        std::process::exit(1);
    }
}
