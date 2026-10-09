//! Pure Apple container CLI arguments and guest session policy.

use std::path::{Component, Path, PathBuf};

use crate::channel::WireSpawnSpec;
use crate::workload::HostRequest;
use crate::workload::{Isolation, MountMode, NetworkPolicy, Runtime, StableId, WorkloadSpec};
use serde::{Deserialize, Serialize};

pub const GUEST_ROOT: &str = "/run/hypervisor";
pub const GUEST_AGENT: &str = "/run/hypervisor/agent";
pub const GUEST_METADATA: &str = "/run/hypervisor/workload.json";
pub const GUEST_SOCKET: &str = "/run/hypervisor/agent.sock";
pub const REVIEWER_WRITE_PATHS: &[&str] = &["/cache", "/tmp", "/dev"];

/// A CLI invocation without its executable, suitable for direct `Command::args`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation(pub Vec<String>);

/// First line on the published guest socket; a terminal stream follows `Attach`.
#[derive(Serialize, Deserialize)]
pub enum GuestRequest {
    Control(HostRequest),
    Attach(PathBuf),
}

/// Stable names are short enough for container, volume, and socket paths.
#[must_use]
pub fn name(id: &StableId) -> String {
    format!("hv-{}-{}", id.host, id.local)
}

/// Refuse controls this CLI driver cannot enforce and ambiguous mount syntax.
///
/// # Errors
/// Invalid isolation, mount, image, network, identity, or resource request.
pub fn validate(spec: &WorkloadSpec) -> Result<(), &'static str> {
    super::workload::validate_common(spec)?;
    if spec.runtime != Runtime::AppleContainer || spec.isolation != Isolation::VirtualMachine {
        return Err("container driver requires apple_container and virtual_machine isolation");
    }
    if name(&spec.id).len() > 63 {
        return Err("container name exceeds 63 bytes");
    }
    if spec.image.as_ref().is_none_or(|image| {
        image.is_empty() || image.starts_with('-') || image.contains(['\0', '\n'])
    }) {
        return Err("container image is required");
    }
    if spec.network == NetworkPolicy::Host {
        return Err("container driver requires network isolation");
    }
    if !spec.credential_refs.is_empty() {
        return Err("container driver cannot inject credentials");
    }
    if spec.resources.memory_bytes == Some(0) || spec.resources.cpu_count == Some(0) {
        return Err("container resource limit must be positive");
    }
    for mount in &spec.mounts {
        if !mount.source.is_absolute() || !mount.target.is_absolute() {
            return Err("container mounts must be absolute");
        }
        if mount
            .target
            .components()
            .any(|part| part == Component::ParentDir)
        {
            return Err("container mount target cannot traverse parents");
        }
        if [mount.source.as_path(), mount.target.as_path()]
            .iter()
            .any(|path| path.to_str().is_none_or(|s| s.contains([',', '\0', '\n'])))
        {
            return Err("container mount path cannot use CLI separators");
        }
        if ["/workspace", "/cache", GUEST_ROOT].iter().any(|root| {
            mount.target.starts_with(root) || Path::new(root).starts_with(&mount.target)
        }) {
            return Err("container mount overlaps a driver-owned target");
        }
    }
    if [spec.workspace_dir.as_path(), spec.cache_dir.as_path()]
        .iter()
        .any(|path| path.to_str().is_none_or(|s| s.contains([',', '\0', '\n'])))
    {
        return Err("container path cannot use CLI separators");
    }
    Ok(())
}

/// Build the one persistent workspace container. `agent` is a Linux executable supplied by the caller.
///
/// # Errors
/// Invalid workload or host paths.
pub fn run(
    spec: &WorkloadSpec,
    agent: &Path,
    metadata: &Path,
    host_socket: &Path,
) -> Result<Invocation, &'static str> {
    validate(spec)?;
    let paths = [agent, metadata, host_socket];
    if paths.iter().any(|path| {
        !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|s| s.contains([',', ':', '\0', '\n']))
    }) {
        return Err("driver path is not an absolute CLI-safe path");
    }
    let mut args = vec![
        "run".into(),
        "--detach".into(),
        "--init".into(),
        "--name".into(),
        name(&spec.id),
        "--user".into(),
        "0".into(),
        // RFC-37 runs 8 and 31: --internal still reaches host services and DNS.
        "--network".into(),
        "none".into(),
        "--entrypoint".into(),
        GUEST_AGENT.into(),
        "--publish-socket".into(),
        format!("{}:{GUEST_SOCKET}", host_socket.display()),
        "--mount".into(),
        bind(&spec.workspace_dir, Path::new("/workspace"), false),
        "--mount".into(),
        format!(
            "type=volume,source={},target=/cache",
            cache_volume(&spec.id)
        ),
        "--mount".into(),
        bind(agent, Path::new(GUEST_AGENT), true),
        "--mount".into(),
        bind(metadata, Path::new(GUEST_METADATA), true),
    ];
    for mount in &spec.mounts {
        // Apple container 1.4.1 requires --volume for a Unix socket file.
        let value = format!("{}:{}", mount.source.display(), mount.target.display());
        if mount.target == Path::new("/run/broker.sock") {
            args.extend(["--volume".into(), value]);
        } else {
            args.extend([
                "--mount".into(),
                bind(
                    &mount.source,
                    &mount.target,
                    mount.mode == MountMode::ReadOnly,
                ),
            ]);
        }
    }
    if let Some(memory) = spec.resources.memory_bytes {
        args.extend(["--memory".into(), memory.to_string()]);
    }
    if let Some(cpus) = spec.resources.cpu_count {
        args.extend(["--cpus".into(), cpus.to_string()]);
    }
    args.extend([
        spec.image.clone().ok_or("container image is required")?,
        "guest".into(),
        GUEST_METADATA.into(),
        GUEST_ROOT.into(),
    ]);
    Ok(Invocation(args))
}

fn bind(source: &Path, target: &Path, readonly: bool) -> String {
    let mut mount = format!(
        "type=bind,source={},target={}",
        source.display(),
        target.display()
    );
    if readonly {
        mount.push_str(",readonly");
    }
    mount
}

#[must_use]
pub fn cache_volume(id: &StableId) -> String {
    format!("{}-cache", name(id))
}

/// Make a metadata copy whose paths are meaningful inside the guest.
#[must_use]
pub fn guest_spec(spec: &WorkloadSpec) -> WorkloadSpec {
    let mut guest = spec.clone();
    guest.workspace_dir = PathBuf::from("/workspace");
    guest.cache_dir = PathBuf::from("/cache");
    guest
}

/// Numeric guest users are explicit; the reviewer role is a separate, read-only policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestUser {
    Writer(u32),
    Reviewer(u32),
}

/// # Errors
/// Missing, root, or malformed guest user.
pub fn session_user(spec: &WireSpawnSpec) -> Result<GuestUser, &'static str> {
    let user = spec
        .user
        .as_deref()
        .ok_or("container session needs a guest user")?;
    let (reviewer, uid) = match user.strip_prefix("reviewer:") {
        Some(uid) => (true, uid),
        None => (false, user),
    };
    let uid: u32 = uid.parse().map_err(|_| "guest user must be numeric")?;
    if !(10_000..=60_000).contains(&uid) {
        return Err("guest user must be an unprivileged uid from 10000 to 60000");
    }
    Ok(if reviewer {
        GuestUser::Reviewer(uid)
    } else {
        GuestUser::Writer(uid)
    })
}

/// Distinct session UIDs prevent one guest session from signalling another.
///
/// # Errors
/// The requested UID already belongs to a live session.
pub fn admit_user(user: GuestUser, existing: &[u32]) -> Result<(), &'static str> {
    let uid = match user {
        GuestUser::Writer(uid) | GuestUser::Reviewer(uid) => uid,
    };
    if existing.contains(&uid) {
        Err("guest session uid already in use")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::SessionKind;
    use crate::workload::{Mount, ResourceLimits};
    use proptest::prelude::*;

    fn spec() -> WorkloadSpec {
        WorkloadSpec {
            id: StableId {
                host: "test".into(),
                local: "ws".into(),
            },
            runtime: Runtime::AppleContainer,
            isolation: Isolation::VirtualMachine,
            image: Some("local/base:1".into()),
            mounts: vec![],
            env: vec![],
            credential_refs: vec![],
            network: NetworkPolicy::Isolated,
            resources: ResourceLimits {
                memory_bytes: None,
                cpu_count: None,
            },
            workspace_dir: "/tmp/ws".into(),
            cache_dir: "/tmp/cache".into(),
        }
    }

    #[test]
    fn run_command_golden() {
        assert_eq!(
            run(
                &spec(),
                Path::new("/tmp/agent"),
                Path::new("/tmp/spec"),
                Path::new("/tmp/host.sock")
            )
            .unwrap()
            .0,
            [
                "run",
                "--detach",
                "--init",
                "--name",
                "hv-test-ws",
                "--user",
                "0",
                "--network",
                "none",
                "--entrypoint",
                GUEST_AGENT,
                "--publish-socket",
                "/tmp/host.sock:/run/hypervisor/agent.sock",
                "--mount",
                "type=bind,source=/tmp/ws,target=/workspace",
                "--mount",
                "type=volume,source=hv-test-ws-cache,target=/cache",
                "--mount",
                "type=bind,source=/tmp/agent,target=/run/hypervisor/agent,readonly",
                "--mount",
                "type=bind,source=/tmp/spec,target=/run/hypervisor/workload.json,readonly",
                "local/base:1",
                "guest",
                GUEST_METADATA,
                GUEST_ROOT
            ]
        );
    }

    #[test]
    fn rejects_host_network_and_reserved_mount() {
        let mut request = spec();
        request.network = NetworkPolicy::Host;
        assert!(validate(&request).is_err());
        request.network = NetworkPolicy::Deny;
        request.mounts.push(Mount {
            source: "/tmp/data".into(),
            target: "/workspace/x".into(),
            mode: MountMode::ReadWrite,
        });
        assert!(validate(&request).is_err());
    }

    #[test]
    fn validate_refusal_cases() {
        type Case = (&'static str, fn(&mut WorkloadSpec));
        let cases: &[Case] = &[
            ("runtime", |s| s.runtime = Runtime::Host),
            ("isolation", |s| s.isolation = Isolation::None),
            ("name", |s| s.id.local = "x".repeat(60)),
            ("image missing", |s| s.image = None),
            ("image empty", |s| s.image = Some(String::new())),
            ("image option", |s| s.image = Some("-image".into())),
            ("image nul", |s| s.image = Some("a\0b".into())),
            ("image newline", |s| s.image = Some("a\nb".into())),
            ("credentials", |s| s.credential_refs.push("secret".into())),
            ("memory", |s| s.resources.memory_bytes = Some(0)),
            ("cpu", |s| s.resources.cpu_count = Some(0)),
            ("relative source", |s| {
                s.mounts.push(mount("relative", "/deps"));
            }),
            ("relative target", |s| {
                s.mounts.push(mount("/tmp/data", "deps"));
            }),
            ("parent target", |s| {
                s.mounts.push(mount("/tmp/data", "/deps/../other"));
            }),
            ("source separator", |s| {
                s.mounts.push(mount("/tmp/a,b", "/deps"));
            }),
            ("target nul", |s| s.mounts.push(mount("/tmp/data", "/a\0b"))),
            ("target newline", |s| {
                s.mounts.push(mount("/tmp/data", "/a\nb"));
            }),
            ("owned target", |s| {
                s.mounts.push(mount("/tmp/data", "/cache/x"));
            }),
            ("owned ancestor", |s| {
                s.mounts.push(mount("/tmp/data", "/run"));
            }),
            ("root target", |s| s.mounts.push(mount("/tmp/data", "/"))),
            ("workspace separator", |s| {
                s.workspace_dir = "/tmp/a,b".into();
            }),
            ("cache separator", |s| s.cache_dir = "/tmp/a\nb".into()),
        ];
        for (label, change) in cases {
            let mut request = spec();
            change(&mut request);
            assert!(validate(&request).is_err(), "{label}");
        }
    }

    fn mount(source: &str, target: &str) -> Mount {
        Mount {
            source: source.into(),
            target: target.into(),
            mode: MountMode::ReadOnly,
        }
    }

    #[test]
    fn invalid_guest_users() {
        let mut session = WireSpawnSpec {
            command: "sh".into(),
            args: vec![],
            env: vec![],
            cwd: None,
            user: None,
            kind: SessionKind::Shell,
        };
        assert!(session_user(&session).is_err());
        for user in [
            "abc",
            "reviewer:abc",
            "9999",
            "60001",
            "reviewer:9999",
            "reviewer:60001",
        ] {
            session.user = Some(user.into());
            assert!(session_user(&session).is_err(), "{user}");
        }
    }

    #[test]
    fn workload_environment_stays_out_of_process_arguments() {
        let mut request = spec();
        request.env.push(("PRIVATE".into(), "sentinel".into()));
        assert!(
            !run(
                &request,
                Path::new("/tmp/agent"),
                Path::new("/tmp/spec"),
                Path::new("/tmp/host.sock")
            )
            .unwrap()
            .0
            .join(" ")
            .contains("sentinel")
        );
    }

    #[test]
    fn reviewer_role_is_explicit() {
        let session = WireSpawnSpec {
            command: "sh".into(),
            args: vec![],
            env: vec![],
            cwd: None,
            user: Some("reviewer:10001".into()),
            kind: SessionKind::Shell,
        };
        assert_eq!(session_user(&session), Ok(GuestUser::Reviewer(10001)));
        assert!(admit_user(GuestUser::Reviewer(10001), &[10001]).is_err());
    }

    proptest! {
        #[test]
        fn mount_cli_separator_is_rejected(part in "[a-z]{1,10},[a-z]{1,10}") {
            let mut request = spec();
            request.mounts.push(mount(&format!("/tmp/{part}"), "/deps"));
            prop_assert!(validate(&request).is_err());
        }
        #[test]
        fn every_admitted_run_has_no_network(local in "[a-z][a-z0-9]{0,20}") {
            let mut request = spec();
            request.id.local = local;
            let args = run(&request, Path::new("/tmp/agent"), Path::new("/tmp/spec"), Path::new("/tmp/host.sock")).unwrap().0;
            let network = args.windows(2).find(|pair| pair[0] == "--network").unwrap();
            prop_assert_eq!(network[1].as_str(), "none");
        }

        #[test]
        fn a_live_uid_cannot_be_reused(uid in 10_000_u32..=60_000) {
            prop_assert!(admit_user(GuestUser::Writer(uid), &[uid]).is_err());
            prop_assert!(admit_user(GuestUser::Reviewer(uid), &[uid]).is_err());
            prop_assert!(admit_user(GuestUser::Writer(uid), &[]).is_ok());
        }
    }
}
