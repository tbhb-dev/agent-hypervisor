//! Host daemon shell. It prints its version and exits; the session holder arrives in RFC-36 run 9.

fn main() {
    println!(
        "{}",
        hypervisor_core::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
    );
}
