//! Plain values and decisions shared by runtime drivers.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A stable host-scoped name supplied by the caller, never a process ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StableId {
    pub host: String,
    pub local: String,
}

impl StableId {
    /// Check both components before using an ID in a path or control request.
    ///
    /// # Errors
    /// Empty, long, or non-ASCII-safe components are refused.
    pub fn validate(&self) -> Result<(), &'static str> {
        for part in [&self.host, &self.local] {
            if part.is_empty()
                || part.len() > 64
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
                return Err("invalid stable ID component");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn label(&self) -> String {
        format!("{}:{}", self.host, self.local)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Host,
    Seatbelt,
    AppleContainer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    None,
    Seatbelt,
    VirtualMachine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub mode: MountMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Host,
    Deny,
    Isolated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    pub cpu_count: Option<u16>,
}

/// Driver-neutral workload request. Paths are canonicalized by the shell before storage.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadSpec {
    pub id: StableId,
    pub runtime: Runtime,
    pub isolation: Isolation,
    pub image: Option<String>,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub credential_refs: Vec<String>,
    pub network: NetworkPolicy,
    pub resources: ResourceLimits,
    pub workspace_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl fmt::Debug for WorkloadSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadSpec")
            .field("id", &self.id)
            .field("runtime", &self.runtime)
            .field("isolation", &self.isolation)
            .field("image", &self.image)
            .field("mounts", &self.mounts)
            .field(
                "env_names",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("credential_refs", &self.credential_refs)
            .field("network", &self.network)
            .field("resources", &self.resources)
            .field("workspace_dir", &self.workspace_dir)
            .field("cache_dir", &self.cache_dir)
            .finish()
    }
}

/// Which persisted workload can be adopted on this host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Running,
    Stopped,
    Foreign,
}

#[must_use]
pub fn recovery(expected_host: &str, id: &StableId, shim_responded: bool) -> Recovery {
    if id.host != expected_host {
        Recovery::Foreign
    } else if shim_responded {
        Recovery::Running
    } else {
        Recovery::Stopped
    }
}

/// Host driver policy: unsupported isolation requests fail rather than silently weakening them.
///
/// # Errors
/// An unsupported field or an invalid path or ID.
pub fn validate_host(spec: &WorkloadSpec) -> Result<(), &'static str> {
    spec.id.validate()?;
    if spec.runtime != Runtime::Host || spec.isolation != Isolation::None {
        return Err("host driver requires host runtime and no isolation");
    }
    if spec.image.is_some() || !spec.mounts.is_empty() || !spec.credential_refs.is_empty() {
        return Err("host driver cannot apply image, mounts, or credentials");
    }
    if spec.network != NetworkPolicy::Host
        || spec.resources.memory_bytes.is_some()
        || spec.resources.cpu_count.is_some()
    {
        return Err("host driver cannot enforce network or resource limits");
    }
    if !spec.workspace_dir.is_absolute() || !spec.cache_dir.is_absolute() {
        return Err("workload paths must be absolute");
    }
    if spec.workspace_dir.to_str().is_none() || spec.cache_dir.to_str().is_none() {
        return Err("workload paths must be UTF-8");
    }
    for (name, value) in &spec.env {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err("invalid workload environment");
        }
        if name == "WORKSPACE_DIR" || name == "CACHE_DIR" {
            return Err("path environment is driver-owned");
        }
    }
    Ok(())
}

/// Build the whole child environment from the workload and session inputs.
#[must_use]
pub fn session_env(spec: &WorkloadSpec, session_env: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = spec.env.clone();
    env.extend(
        session_env
            .iter()
            .filter(|(name, _)| name != "WORKSPACE_DIR" && name != "CACHE_DIR")
            .cloned(),
    );
    env.push(("WORKSPACE_DIR".into(), path_string(&spec.workspace_dir)));
    env.push(("CACHE_DIR".into(), path_string(&spec.cache_dir)));
    env
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn host() -> WorkloadSpec {
        WorkloadSpec {
            id: StableId {
                host: "h1".into(),
                local: "w1".into(),
            },
            runtime: Runtime::Host,
            isolation: Isolation::None,
            image: None,
            mounts: vec![],
            env: vec![],
            credential_refs: vec![],
            network: NetworkPolicy::Host,
            resources: ResourceLimits {
                memory_bytes: None,
                cpu_count: None,
            },
            workspace_dir: "/work".into(),
            cache_dir: "/cache".into(),
        }
    }

    #[test]
    fn host_accepts_plain_spec_and_rejects_unsupported_controls() {
        let mut spec = host();
        assert_eq!(validate_host(&spec), Ok(()));
        spec.network = NetworkPolicy::Deny;
        assert!(validate_host(&spec).is_err());
        spec.network = NetworkPolicy::Host;
        spec.isolation = Isolation::Seatbelt;
        assert!(validate_host(&spec).is_err());
    }

    #[test]
    fn host_rejects_other_runtimes() {
        for runtime in [Runtime::Seatbelt, Runtime::AppleContainer] {
            let mut spec = host();
            spec.runtime = runtime;
            assert!(validate_host(&spec).is_err());
        }
    }

    #[test]
    fn host_rejects_each_unsupported_field() {
        let mut spec = host();
        spec.image = Some("image".into());
        assert!(validate_host(&spec).is_err());

        let mut spec = host();
        spec.mounts.push(Mount {
            source: "/source".into(),
            target: "/target".into(),
            mode: MountMode::ReadOnly,
        });
        assert!(validate_host(&spec).is_err());

        let mut spec = host();
        spec.credential_refs.push("credential".into());
        assert!(validate_host(&spec).is_err());
    }

    #[test]
    fn host_rejects_each_resource_limit() {
        let mut spec = host();
        spec.resources.memory_bytes = Some(1024);
        assert!(validate_host(&spec).is_err());

        let mut spec = host();
        spec.resources.cpu_count = Some(1);
        assert!(validate_host(&spec).is_err());
    }

    #[test]
    fn host_rejects_relative_workspace_and_cache_paths() {
        let mut spec = host();
        spec.workspace_dir = "relative".into();
        assert!(validate_host(&spec).is_err());

        let mut spec = host();
        spec.cache_dir = "relative".into();
        assert!(validate_host(&spec).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_rejects_non_utf8_workspace_and_cache_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let invalid = PathBuf::from(OsString::from_vec(b"/invalid\xff".to_vec()));
        let mut spec = host();
        spec.workspace_dir = invalid.clone();
        assert!(validate_host(&spec).is_err());

        let mut spec = host();
        spec.cache_dir = invalid;
        assert!(validate_host(&spec).is_err());
    }

    #[test]
    fn host_rejects_driver_owned_environment_names() {
        for name in ["WORKSPACE_DIR", "CACHE_DIR"] {
            let mut spec = host();
            spec.env.push((name.into(), "caller-value".into()));
            assert!(validate_host(&spec).is_err());
        }
    }

    #[test]
    fn host_rejects_invalid_environment_pairs() {
        for (name, value) in [
            ("", "value"),
            ("A=B", "value"),
            ("A\0B", "value"),
            ("A", "v\0x"),
        ] {
            let mut spec = host();
            spec.env.push((name.into(), value.into()));
            assert!(validate_host(&spec).is_err());
        }
    }

    #[test]
    fn recovery_requires_matching_host_and_live_handshake() {
        let id = host().id;
        assert_eq!(recovery("h1", &id, true), Recovery::Running);
        assert_eq!(recovery("h1", &id, false), Recovery::Stopped);
        assert_eq!(recovery("other", &id, true), Recovery::Foreign);
    }

    #[test]
    fn path_variables_override_caller_values() {
        let env = session_env(
            &host(),
            &[
                ("WORKSPACE_DIR".into(), "wrong".into()),
                ("X".into(), "1".into()),
            ],
        );
        assert_eq!(
            env.iter()
                .filter(|(name, _)| name == "WORKSPACE_DIR")
                .count(),
            1
        );
        assert!(env.contains(&("WORKSPACE_DIR".into(), "/work".into())));
        assert!(env.contains(&("X".into(), "1".into())));
    }

    #[test]
    fn debug_hides_environment_values() {
        let mut spec = host();
        spec.env
            .push(("API_KEY".into(), "sensitive-test-value".into()));
        let debug = format!("{spec:?}");
        assert!(debug.contains("API_KEY"));
        assert!(!debug.contains("sensitive-test-value"));
    }

    proptest! {
        #[test]
        fn invalid_stable_id_components_are_refused(
            invalid in prop_oneof![
                Just(String::new()),
                "[a-z]{65,70}",
                "[a-z]{0,8}[/.:\\x00][a-z]{0,8}",
            ]
        ) {
            let mut id = host().id;
            id.host = invalid.clone();
            prop_assert!(id.validate().is_err());
            id.host = "h1".into();
            id.local = invalid;
            prop_assert!(id.validate().is_err());
        }

        #[test]
        fn safe_ids_never_contain_path_separators(host in "[a-zA-Z0-9_-]{1,64}", local in "[a-zA-Z0-9_-]{1,64}") {
            let id = StableId { host, local };
            prop_assert_eq!(id.validate(), Ok(()));
            prop_assert!(!id.label().contains('/'));
        }

        #[test]
        fn injected_paths_cannot_override_driver_paths(value in ".*") {
            let env = session_env(&host(), &[("CACHE_DIR".into(), value)]);
            prop_assert_eq!(env.iter().filter(|(name, _)| name == "CACHE_DIR").count(), 1);
        }
    }
}
