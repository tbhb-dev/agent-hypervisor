//! Cleanup and run-scoped accounting for shell fixtures in integration tests.

use std::fs::OpenOptions;
use std::io::Write;

use rustix::process::{self, Pid, Signal};

pub struct ProcessGroup {
    pid: Pid,
}

impl ProcessGroup {
    pub fn new(pid: i32) -> Self {
        let group = Self {
            pid: Pid::from_raw(pid).unwrap(),
        };
        if let Some(path) = std::env::var_os("HYPERVISOR_TEST_GROUPS") {
            let mut file = OpenOptions::new().append(true).open(path).unwrap();
            writeln!(file, "{pid}").unwrap();
        }
        group
    }

    pub fn kill(&self) {
        // The test may already have reaped the leader, while a background job remains.
        let _ = process::kill_process_group(self.pid, Signal::KILL);
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}
