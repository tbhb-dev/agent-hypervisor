//! Cleanup and run-scoped accounting for shell fixtures in integration tests.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use rustix::process::{self, Pid, Signal};

pub struct ProcessGroup {
    pid: Pid,
    armed: bool,
}

impl ProcessGroup {
    pub fn new(pid: i32) -> Self {
        if let Some(path) = std::env::var_os("HYPERVISOR_TEST_GROUPS") {
            return Self::new_in(pid, Path::new(&path));
        }
        Self {
            pid: Pid::from_raw(pid).unwrap(),
            armed: true,
        }
    }

    pub fn new_in(pid: i32, path: &Path) -> Self {
        record(path, pid);
        Self {
            pid: Pid::from_raw(pid).unwrap(),
            armed: true,
        }
    }

    pub fn kill(&mut self) -> bool {
        if !self.armed {
            return false;
        }
        self.armed = false;
        // A surviving member pins the group ID even after the leader is reaped.
        if process::test_kill_process_group(self.pid).is_ok() {
            let _ = process::kill_process_group(self.pid, Signal::KILL);
            return true;
        }
        false
    }
}

fn record(path: &Path, pid: i32) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(format!("{pid}\n").as_bytes()).unwrap();
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::record;
    use std::fs::{self, OpenOptions};
    use std::thread;

    #[test]
    fn concurrent_records_remain_separate_lines() {
        let path = std::env::temp_dir().join(format!("hypervisor-records-{}", std::process::id()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let workers: Vec<_> = (0..16)
            .map(|worker| {
                let path = path.clone();
                thread::spawn(move || {
                    for offset in 0..500 {
                        record(&path, 100_000 + worker * 500 + offset);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let records = fs::read_to_string(&path).unwrap();
        assert_eq!(records.lines().count(), 8_000);
        assert!(records.lines().all(|line| {
            line.parse::<i32>()
                .is_ok_and(|pid| (100_000..108_000).contains(&pid))
        }));
        fs::remove_file(path).unwrap();
    }
}
