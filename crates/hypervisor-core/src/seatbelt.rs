//! Seatbelt driver decisions: admission and the `sandbox-exec` profile for a workload.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::workload::{self, Isolation, MountMode, NetworkPolicy, Runtime, WorkloadSpec};

/// The system binary that applies a profile and then executes the session command.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Credential stores under the home directory that stay denied even when a mount covers them.
pub const SECRET_PATHS: [&str; 7] = [
    ".ssh",
    ".gnupg",
    ".aws",
    ".netrc",
    ".config/gh",
    ".codex/auth.json",
    "Library/Keychains",
];

/// Environment names that point a process at an agent or daemon socket; seatbelt sessions drop them.
pub const AGENT_ENV: [&str; 5] = [
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "SSH_ASKPASS",
    "GPG_AGENT_INFO",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// The system resolver's socket, which host-network sessions need for name lookups.
pub const RESOLVER_SOCKET: &str = "/private/var/run/mDNSResponder";

/// Keychain services a session may not look up, so no credential prompt can appear.
const KEYCHAIN_SERVICES: [&str; 4] = [
    "com.apple.SecurityServer",
    "com.apple.securityd.xpc",
    "com.apple.security.agent",
    "com.apple.security.authhost",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    /// Connecting to a Unix socket at the path. Rules never mix it with file access.
    Connect,
}

impl Access {
    fn operation(self) -> &'static str {
        match self {
            Self::Read => "file-read-data",
            Self::Write => "file-write*",
            Self::Connect => "network-outbound",
        }
    }
}

/// One path rule. Later rules win, as in Seatbelt, and paths match by `subpath`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRule {
    pub allow: bool,
    pub access: Vec<Access>,
    pub paths: Vec<PathBuf>,
}

/// Check a Seatbelt workload before the shell persists it.
///
/// # Errors
/// A control Seatbelt cannot enforce, or a mount that remaps its path.
pub fn validate(spec: &WorkloadSpec) -> Result<(), &'static str> {
    if spec.runtime != Runtime::Seatbelt || spec.isolation != Isolation::Seatbelt {
        return Err("seatbelt driver requires seatbelt runtime and isolation");
    }
    if spec.image.is_some() || !spec.credential_refs.is_empty() {
        return Err("seatbelt driver cannot apply an image or credentials");
    }
    if spec.network == NetworkPolicy::Isolated
        || spec.resources.memory_bytes.is_some()
        || spec.resources.cpu_count.is_some()
    {
        return Err("seatbelt driver cannot enforce isolated network or resource limits");
    }
    for mount in &spec.mounts {
        if mount.source != mount.target || !mount.source.is_absolute() {
            return Err("seatbelt mounts must name one absolute path");
        }
        if mount.source.to_str().is_none() {
            return Err("workload paths must be UTF-8");
        }
    }
    workload::validate_common(spec)
}

/// The ordered path rules for a workload, given the user's home and the driver's private root.
#[must_use]
pub fn path_rules(spec: &WorkloadSpec, home: &Path, root: &Path) -> Vec<PathRule> {
    let rule = |allow, access: &[Access], paths: Vec<PathBuf>| PathRule {
        allow,
        access: access.to_vec(),
        paths,
    };
    let roots = [spec.workspace_dir.clone(), spec.cache_dir.clone()];
    let mounts = |writable_only: bool| {
        spec.mounts
            .iter()
            .filter(move |mount| !writable_only || mount.mode == MountMode::ReadWrite)
            .map(|mount| mount.source.clone())
    };
    let mut writable = vec![PathBuf::from("/dev")];
    writable.extend(roots.iter().cloned().chain(mounts(true)));
    let mut readable = roots.to_vec();
    readable.extend(mounts(false));
    let mut secret: Vec<PathBuf> = SECRET_PATHS.iter().map(|path| home.join(path)).collect();
    secret.push(root.to_path_buf());
    let both = [Access::Read, Access::Write];
    let mut sockets = roots.to_vec();
    if spec.network == NetworkPolicy::Host {
        sockets.push(PathBuf::from(RESOLVER_SOCKET));
    }
    vec![
        rule(false, &[Access::Write], vec![PathBuf::from("/")]),
        rule(true, &[Access::Write], writable),
        rule(false, &[Access::Read], vec![home.to_path_buf()]),
        rule(true, &[Access::Read], readable),
        rule(false, &both, secret),
        rule(false, &[Access::Connect], vec![PathBuf::from("/")]),
        rule(true, &[Access::Connect], sockets),
        rule(false, &[Access::Connect], vec![root.to_path_buf()]),
    ]
}

/// Decide an access the way Seatbelt evaluates [`path_rules`] over its allow-default base.
#[must_use]
pub fn allowed(rules: &[PathRule], access: Access, path: &Path) -> bool {
    rules
        .iter()
        .rev()
        .find(|rule| {
            rule.access.contains(&access) && rule.paths.iter().any(|base| path.starts_with(base))
        })
        .is_none_or(|rule| rule.allow)
}

/// Render the profile text passed to `sandbox-exec -p`.
#[must_use]
pub fn profile(spec: &WorkloadSpec, home: &Path, root: &Path) -> String {
    let mut text = String::from("(version 1)\n(allow default)\n");
    for rule in path_rules(spec, home, root) {
        let verb = if rule.allow { "allow" } else { "deny" };
        let operations: Vec<_> = rule
            .access
            .iter()
            .map(|access| access.operation())
            .collect();
        let _ = write!(text, "({verb} {}", operations.join(" "));
        for path in &rule.paths {
            if rule.access.contains(&Access::Connect) {
                let _ = write!(text, " (remote unix-socket (subpath {}))", quote(path));
            } else {
                let _ = write!(text, " (subpath {})", quote(path));
            }
        }
        text.push_str(")\n");
    }
    if spec.network == NetworkPolicy::Deny {
        text.push_str("(deny network*)\n");
    }
    text.push_str("(deny mach-lookup");
    for service in KEYCHAIN_SERVICES {
        let _ = write!(text, " (global-name \"{service}\")");
    }
    text.push_str(")\n(deny lsopen)\n(deny appleevent-send)\n");
    text
}

/// The program and arguments a session runs; Seatbelt workloads run under the profile.
#[must_use]
pub fn session_command(
    spec: &WorkloadSpec,
    home: &Path,
    root: &Path,
    command: String,
    args: Vec<String>,
) -> (String, Vec<String>) {
    if spec.runtime != Runtime::Seatbelt {
        return (command, args);
    }
    let mut wrapped = vec!["-p".into(), profile(spec, home, root), "--".into(), command];
    wrapped.extend(args);
    (SANDBOX_EXEC.into(), wrapped)
}

/// Drop agent and daemon socket variables from a seatbelt session's environment.
#[must_use]
pub fn session_env(spec: &WorkloadSpec, env: Vec<(String, String)>) -> Vec<(String, String)> {
    if spec.runtime != Runtime::Seatbelt {
        return env;
    }
    env.into_iter()
        .filter(|(name, _)| !AGENT_ENV.contains(&name.as_str()))
        .collect()
}

fn quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{Mount, ResourceLimits, StableId};
    use proptest::prelude::*;

    fn spec() -> WorkloadSpec {
        WorkloadSpec {
            id: StableId {
                host: "h1".into(),
                local: "w1".into(),
            },
            runtime: Runtime::Seatbelt,
            isolation: Isolation::Seatbelt,
            image: None,
            mounts: vec![Mount {
                source: "/opt/tools".into(),
                target: "/opt/tools".into(),
                mode: MountMode::ReadOnly,
            }],
            env: vec![],
            credential_refs: vec![],
            network: NetworkPolicy::Deny,
            resources: ResourceLimits {
                memory_bytes: None,
                cpu_count: None,
            },
            workspace_dir: "/Users/u/ws".into(),
            cache_dir: "/Users/u/cache".into(),
        }
    }

    const GOLDEN: &str = r#"(version 1)
(allow default)
(deny file-write* (subpath "/"))
(allow file-write* (subpath "/dev") (subpath "/Users/u/ws") (subpath "/Users/u/cache"))
(deny file-read-data (subpath "/Users/u"))
(allow file-read-data (subpath "/Users/u/ws") (subpath "/Users/u/cache") (subpath "/opt/tools"))
(deny file-read-data file-write* (subpath "/Users/u/.ssh") (subpath "/Users/u/.gnupg") (subpath "/Users/u/.aws") (subpath "/Users/u/.netrc") (subpath "/Users/u/.config/gh") (subpath "/Users/u/.codex/auth.json") (subpath "/Users/u/Library/Keychains") (subpath "/r"))
(deny network-outbound (remote unix-socket (subpath "/")))
(allow network-outbound (remote unix-socket (subpath "/Users/u/ws")) (remote unix-socket (subpath "/Users/u/cache")))
(deny network-outbound (remote unix-socket (subpath "/r")))
(deny network*)
(deny mach-lookup (global-name "com.apple.SecurityServer") (global-name "com.apple.securityd.xpc") (global-name "com.apple.security.agent") (global-name "com.apple.security.authhost"))
(deny lsopen)
(deny appleevent-send)
"#;

    #[test]
    fn profile_matches_golden_text() {
        assert_eq!(profile(&spec(), "/Users/u".as_ref(), "/r".as_ref()), GOLDEN);
    }

    #[test]
    fn host_network_omits_the_network_deny() {
        let mut spec = spec();
        spec.network = NetworkPolicy::Host;
        let text = profile(&spec, "/Users/u".as_ref(), "/r".as_ref());
        assert!(!text.contains("(deny network*)"));
        assert!(text.contains("(remote unix-socket (subpath \"/r\"))"));
        let rules = path_rules(&spec, "/Users/u".as_ref(), "/r".as_ref());
        assert!(allowed(&rules, Access::Connect, RESOLVER_SOCKET.as_ref()));
    }

    #[test]
    fn sockets_connect_only_inside_workspace_and_cache() {
        let rules = path_rules(&spec(), "/Users/u".as_ref(), "/r".as_ref());
        let can = |path: &str| allowed(&rules, Access::Connect, path.as_ref());
        assert!(can("/Users/u/ws/a.sock"));
        assert!(!can(RESOLVER_SOCKET));
        assert!(!can("/private/tmp/com.apple.launchd.x/Listeners"));
    }

    #[test]
    fn seatbelt_sessions_drop_agent_environment() {
        let pair = |name: &str, value: &str| (name.to_string(), value.to_string());
        let env = vec![pair("SSH_AUTH_SOCK", "/a"), pair("PATH", "/bin")];
        assert_eq!(session_env(&spec(), env.clone()), [pair("PATH", "/bin")]);
        let mut host = spec();
        host.runtime = Runtime::Host;
        assert_eq!(session_env(&host, env.clone()), env);
    }

    #[test]
    fn writable_mounts_join_the_write_roots() {
        let mut spec = spec();
        spec.mounts[0].mode = MountMode::ReadWrite;
        let rules = path_rules(&spec, "/Users/u".as_ref(), "/r".as_ref());
        assert!(allowed(&rules, Access::Write, "/opt/tools/bin".as_ref()));
        assert!(!allowed(&rules, Access::Write, "/opt/other".as_ref()));
    }

    #[test]
    fn rules_follow_subpath_and_last_match_semantics() {
        let rules = path_rules(&spec(), "/Users/u".as_ref(), "/r".as_ref());
        assert!(allowed(&rules, Access::Read, "/Users/u/ws/a".as_ref()));
        assert!(!allowed(&rules, Access::Read, "/Users/u/wsx".as_ref()));
        assert!(!allowed(&rules, Access::Read, "/Users/u/.ssh/id".as_ref()));
        assert!(allowed(&rules, Access::Read, "/etc/hosts".as_ref()));
        assert!(!allowed(&rules, Access::Write, "/tmp/x".as_ref()));
        assert!(allowed(&rules, Access::Write, "/dev/ttys001".as_ref()));
    }

    #[test]
    fn host_runtime_commands_are_unchanged() {
        let mut spec = spec();
        spec.runtime = Runtime::Host;
        let (command, args) = session_command(
            &spec,
            "/h".as_ref(),
            "/r".as_ref(),
            "sh".into(),
            vec!["-c".into()],
        );
        assert_eq!((command.as_str(), args), ("sh", vec!["-c".to_string()]));
    }

    #[test]
    fn seatbelt_commands_run_under_the_profile() {
        let (command, args) = session_command(
            &spec(),
            "/Users/u".as_ref(),
            "/r".as_ref(),
            "-x".into(),
            vec!["a".into()],
        );
        assert_eq!(command, SANDBOX_EXEC);
        assert_eq!(args, ["-p", GOLDEN, "--", "-x", "a"]);
    }

    #[test]
    fn validate_accepts_plain_spec_and_rejects_each_unenforced_control() {
        assert_eq!(validate(&spec()), Ok(()));
        let cases: [fn(&mut WorkloadSpec); 9] = [
            |s| s.runtime = Runtime::Host,
            |s| s.isolation = Isolation::None,
            |s| s.image = Some("image".into()),
            |s| s.credential_refs.push("credential".into()),
            |s| s.network = NetworkPolicy::Isolated,
            |s| s.resources.memory_bytes = Some(1),
            |s| s.resources.cpu_count = Some(1),
            |s| s.mounts[0].target = "/elsewhere".into(),
            |s| s.workspace_dir = "relative".into(),
        ];
        for change in cases {
            let mut spec = spec();
            change(&mut spec);
            assert!(validate(&spec).is_err());
        }
        let mut relative = spec();
        relative.mounts[0].source = "tools".into();
        relative.mounts[0].target = "tools".into();
        assert!(validate(&relative).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn validate_rejects_non_utf8_mounts() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let invalid = PathBuf::from(OsString::from_vec(b"/invalid\xff".to_vec()));
        let mut spec = spec();
        spec.mounts[0].source.clone_from(&invalid);
        spec.mounts[0].target = invalid;
        assert!(validate(&spec).is_err());
    }

    #[test]
    fn workload_validation_dispatches_on_runtime() {
        assert_eq!(workload::validate_workload(&spec()), Ok(()));
        let mut spec = spec();
        spec.runtime = Runtime::AppleContainer;
        assert!(workload::validate_workload(&spec).is_err());
    }

    fn mount_strategy() -> impl Strategy<Value = Mount> {
        ("(/Users/u|/opt)(/[a-z.]{1,6}){0,3}", any::<bool>()).prop_map(|(path, writable)| Mount {
            source: path.clone().into(),
            target: path.into(),
            mode: if writable {
                MountMode::ReadWrite
            } else {
                MountMode::ReadOnly
            },
        })
    }

    proptest! {
        #[test]
        fn secrets_and_runtime_root_stay_denied_under_any_mounts(
            mounts in prop::collection::vec(mount_strategy(), 0..6),
            secret in 0..=SECRET_PATHS.len(),
            tail in "(/[a-z]{1,4}){0,2}",
        ) {
            let mut spec = spec();
            spec.mounts = mounts;
            spec.workspace_dir = "/Users/u".into();
            let rules = path_rules(&spec, "/Users/u".as_ref(), "/Users/u/.hv".as_ref());
            let base = SECRET_PATHS.get(secret).map_or_else(|| PathBuf::from("/Users/u/.hv"), |path| Path::new("/Users/u").join(path));
            let path = PathBuf::from(format!("{}{tail}", base.display()));
            prop_assert!(!allowed(&rules, Access::Read, &path));
            prop_assert!(!allowed(&rules, Access::Write, &path));
        }

        #[test]
        fn writes_outside_write_roots_stay_denied(
            mounts in prop::collection::vec(mount_strategy(), 0..6),
            path in "(/[a-z]{1,6}){1,4}",
        ) {
            let mut spec = spec();
            spec.mounts = mounts.clone();
            let rules = path_rules(&spec, "/Users/u".as_ref(), "/r".as_ref());
            let path = PathBuf::from(path);
            let root = |base: &Path| path.starts_with(base);
            let writable = root("/dev".as_ref())
                || root(&spec.workspace_dir)
                || root(&spec.cache_dir)
                || mounts.iter().any(|mount| mount.mode == MountMode::ReadWrite && root(&mount.source));
            prop_assert_eq!(allowed(&rules, Access::Write, &path), writable && !root("/r".as_ref()));
        }

        #[test]
        fn agent_sockets_outside_workspace_and_cache_are_unreachable(
            mounts in prop::collection::vec(mount_strategy(), 0..6),
            host_network in any::<bool>(),
            path in "(/Users/u|/private/tmp|/opt)(/[a-z. ]{1,8}){1,4}",
        ) {
            let mut spec = spec();
            spec.mounts = mounts;
            if host_network {
                spec.network = NetworkPolicy::Host;
            }
            let rules = path_rules(&spec, "/Users/u".as_ref(), "/r".as_ref());
            let path = PathBuf::from(path);
            let inside = path.starts_with(&spec.workspace_dir) || path.starts_with(&spec.cache_dir);
            prop_assert_eq!(allowed(&rules, Access::Connect, &path), inside);
        }

        #[test]
        fn quoted_paths_cannot_end_their_string(path in "/[ -~]{0,24}") {
            let quoted = quote(Path::new(&path));
            let inner = &quoted[1..quoted.len() - 1];
            let unescaped = inner.replace("\\\\", "").replace("\\\"", "");
            prop_assert!(!unescaped.contains('"'));
            prop_assert!(!unescaped.contains('\\'));
        }
    }
}
