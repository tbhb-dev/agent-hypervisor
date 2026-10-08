//! Plain launchd job values for host shims. The shell runs `launchctl`.

use std::collections::{BTreeMap, BTreeSet};

use crate::workload::{self, Recovery, StableId};

/// Label prefix for product shims; tests use [`test_prefix`].
pub const PRODUCT_PREFIX: &str = "dev.tbhb.hypervisor.shim";

const TEST_OWNER: &str = "dev.tbhb.hypervisor.test.owner-";

/// Test label prefix naming the test process that owns the job, so a later run can find
/// jobs whose owner died without booting out its own or a concurrent run's jobs.
#[must_use]
pub fn test_prefix(owner: u32, fixture: u64) -> String {
    format!("{TEST_OWNER}{owner}-{fixture}")
}

/// Test job labels and their owner PIDs named anywhere in `launchctl print gui/<uid>` output.
#[must_use]
pub fn test_jobs(print: &str) -> BTreeMap<String, u32> {
    print
        .split_whitespace()
        .filter_map(|token| {
            let rest = token.strip_prefix(TEST_OWNER)?;
            let (owner, tail) = rest.split_once('-')?;
            let valid = !owner.is_empty()
                && owner.bytes().all(|byte| byte.is_ascii_digit())
                && !tail.is_empty()
                && tail
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
            valid.then(|| Some((token.to_owned(), owner.parse().ok()?)))?
        })
        .collect()
}

/// Labels to boot out: every test job whose owner is not in `live`.
#[must_use]
pub fn sweep_targets(jobs: &BTreeMap<String, u32>, live: &BTreeSet<u32>) -> Vec<String> {
    jobs.iter()
        .filter(|(_, owner)| !live.contains(owner))
        .map(|(label, _)| label.clone())
        .collect()
}

/// Where and how the host driver registers shims with launchd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobConfig {
    pub prefix: String,
    pub uid: u32,
    pub throttle_seconds: u32,
}

impl JobConfig {
    /// Check the prefix before it names a launchd job.
    ///
    /// # Errors
    /// An empty, long, or non-reverse-DNS prefix.
    pub fn validate(&self) -> Result<(), &'static str> {
        let prefix = &self.prefix;
        if prefix.is_empty()
            || prefix.len() > 128
            || prefix.starts_with('.')
            || prefix.ends_with('.')
            || prefix.contains("..")
            || !prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("invalid launchd label prefix");
        }
        Ok(())
    }

    /// One job per workload, named from its stable ID, never from a PID.
    #[must_use]
    pub fn label(&self, id: &StableId) -> String {
        format!("{}.{}.{}", self.prefix, id.host, id.local)
    }

    /// The per-user GUI domain; the driver never names the system domain.
    #[must_use]
    pub fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    #[must_use]
    pub fn service(&self, id: &StableId) -> String {
        format!("{}/{}", self.domain(), self.label(id))
    }

    /// Property list for `launchctl bootstrap`. launchd restarts a crashed shim but not
    /// one that exited cleanly after a stop request.
    #[must_use]
    pub fn plist(&self, id: &StableId, program: &[&str], stderr: &str) -> String {
        let mut arguments = String::new();
        for argument in program {
            arguments.push_str("<string>");
            arguments.push_str(&escape(argument));
            arguments.push_str("</string>");
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\
             <key>Label</key><string>{}</string>\
             <key>ProgramArguments</key><array>{arguments}</array>\
             <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\
             <key>ThrottleInterval</key><integer>{}</integer>\
             <key>StandardErrorPath</key><string>{}</string>\
             </dict></plist>\n",
            escape(&self.label(id)),
            self.throttle_seconds,
            escape(stderr),
        )
    }
}

/// How the shell registers a shim it is allowed to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartPlan {
    Bootstrap,
    /// A loaded job whose shim does not answer is replaced, not reused.
    BootoutThenBootstrap,
}

/// Decide the launchd steps for `start`.
///
/// # Errors
/// The workload cannot start; see [`workload::admit_start`].
pub fn start_plan(
    recovery: Recovery,
    socket_answers: bool,
    job_loaded: bool,
) -> Result<StartPlan, &'static str> {
    workload::admit_start(recovery, socket_answers)?;
    Ok(if job_loaded {
        StartPlan::BootoutThenBootstrap
    } else {
        StartPlan::Bootstrap
    })
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn config() -> JobConfig {
        JobConfig {
            prefix: "dev.tbhb.hypervisor.test.r18".into(),
            uid: 501,
            throttle_seconds: 1,
        }
    }

    fn id() -> StableId {
        StableId {
            host: "h1".into(),
            local: "w1".into(),
        }
    }

    #[test]
    fn names_job_domain_and_service_from_the_stable_id() {
        let config = config();
        assert_eq!(config.label(&id()), "dev.tbhb.hypervisor.test.r18.h1.w1");
        assert_eq!(config.domain(), "gui/501");
        assert_eq!(
            config.service(&id()),
            "gui/501/dev.tbhb.hypervisor.test.r18.h1.w1"
        );
    }

    #[test]
    fn product_prefix_is_valid_and_separate_from_tests() {
        let config = JobConfig {
            prefix: PRODUCT_PREFIX.into(),
            ..config()
        };
        assert_eq!(config.validate(), Ok(()));
        assert!(!PRODUCT_PREFIX.starts_with("dev.tbhb.hypervisor.test."));
    }

    #[test]
    fn rejects_malformed_prefixes() {
        for prefix in ["", ".a", "a.", "a..b", "a/b", "a b", &"a".repeat(129)] {
            let config = JobConfig {
                prefix: prefix.into(),
                ..config()
            };
            assert!(config.validate().is_err(), "{prefix:?}");
        }
    }

    #[test]
    fn plist_restarts_only_unsuccessful_exits_and_escapes_values() {
        let plist = config().plist(&id(), &["/bin/a&b", "shim", "<x>"], "/tmp/\"e\"");
        assert!(
            plist.contains("<key>Label</key><string>dev.tbhb.hypervisor.test.r18.h1.w1</string>")
        );
        assert!(plist.contains(
            "<array><string>/bin/a&amp;b</string><string>shim</string><string>&lt;x&gt;</string></array>"
        ));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(plist.contains("<key>ThrottleInterval</key><integer>1</integer>"));
        assert!(plist.contains("<string>/tmp/&quot;e&quot;</string>"));
        assert!(!plist.contains("RunAtLoad"));
    }

    #[test]
    fn sweep_finds_test_jobs_and_spares_live_owners() {
        let print = "services = {\n\t0 - dev.tbhb.hypervisor.test.owner-42-0.h.w\n\
                     \t7 0 dev.tbhb.hypervisor.test.owner-43-1.h.w\n\
                     \t0 - dev.tbhb.hypervisor.test.probe\n\
                     \t0 - dev.tbhb.hypervisor.test.owner-x-1.h.w\n\
                     \t0 - dev.tbhb.hypervisor.test.owner-44-\n\
                     \t0 - dev.tbhb.hypervisor.shim.h.w\n}\n\
                     disabled = { \"dev.tbhb.hypervisor.test.owner-42-0.h.w\" => enabled }";
        let jobs = test_jobs(print);
        assert_eq!(
            jobs.into_iter().collect::<Vec<_>>(),
            vec![
                ("dev.tbhb.hypervisor.test.owner-42-0.h.w".to_owned(), 42),
                ("dev.tbhb.hypervisor.test.owner-43-1.h.w".to_owned(), 43),
            ]
        );
        let jobs = test_jobs(print);
        assert_eq!(
            sweep_targets(&jobs, &BTreeSet::from([43])),
            vec!["dev.tbhb.hypervisor.test.owner-42-0.h.w"]
        );
        assert!(sweep_targets(&jobs, &BTreeSet::from([42, 43])).is_empty());
        assert_eq!(test_prefix(42, 0), "dev.tbhb.hypervisor.test.owner-42-0");
    }

    #[test]
    fn start_plan_replaces_a_loaded_silent_job() {
        assert_eq!(
            start_plan(Recovery::Stopped, false, false),
            Ok(StartPlan::Bootstrap)
        );
        assert_eq!(
            start_plan(Recovery::Stopped, false, true),
            Ok(StartPlan::BootoutThenBootstrap)
        );
        assert!(start_plan(Recovery::Restarted, false, true).is_err());
        assert!(start_plan(Recovery::Stopped, true, true).is_err());
    }

    proptest! {
        #[test]
        fn labels_are_injective_for_valid_ids(
            a in "[a-zA-Z0-9_-]{1,16}", b in "[a-zA-Z0-9_-]{1,16}",
            c in "[a-zA-Z0-9_-]{1,16}", d in "[a-zA-Z0-9_-]{1,16}",
        ) {
            let config = config();
            let first = StableId { host: a, local: b };
            let second = StableId { host: c, local: d };
            prop_assert_eq!(config.label(&first) == config.label(&second), first == second);
        }

        #[test]
        fn escaped_values_contain_no_markup(value in ".*") {
            let escaped = escape(&value);
            prop_assert!(!escaped.contains(['<', '>', '"']));
            prop_assert_eq!(escaped.matches('&').count(), escaped.matches("&amp;").count() + escaped.matches("&lt;").count() + escaped.matches("&gt;").count() + escaped.matches("&quot;").count());
        }

        #[test]
        fn sweep_boots_out_exactly_the_dead_owners_jobs(
            owners in prop::collection::btree_map(0u32..1000, any::<bool>(), 0..12),
        ) {
            let print = owners.keys().map(|owner| test_prefix(*owner, 1) + ".h.w").collect::<Vec<_>>().join("\n\t0 - ");
            let live: BTreeSet<u32> = owners.iter().filter(|(_, alive)| **alive).map(|(owner, _)| *owner).collect();
            let jobs = test_jobs(&print);
            prop_assert_eq!(jobs.len(), owners.len());
            let targets = sweep_targets(&jobs, &live);
            prop_assert_eq!(targets.len(), owners.len() - live.len());
            for label in targets {
                prop_assert!(!live.contains(&jobs[&label]));
            }
        }

        #[test]
        fn start_plan_bootouts_exactly_when_loaded(loaded in any::<bool>()) {
            let plan = start_plan(Recovery::Stopped, false, loaded).unwrap();
            prop_assert_eq!(plan == StartPlan::BootoutThenBootstrap, loaded);
        }
    }
}
