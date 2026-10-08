//! Attenuation rules for agent-created workloads (RFC-38 run 14).
//!
//! A workload's grant is a row held by `agentd`, keyed by the workload's SPIFFE ID. A child's row
//! is [`intersect`] of its parent's row and the grant it asked for, so it is never wider than the
//! parent on any axis: isolation, egress, credentials, and mounts. [`within`] is the containment
//! test for one request against one grant, and it fails closed.
//!
//! Credentials appear only as broker references. No type here has a field for a secret value; a
//! child receives a [`BrokerGrant`] naming its own identity and a reference its parent holds.

use std::collections::BTreeSet;

/// How strongly a workload is isolated from the host, weakest first.
///
/// A child may only be created at its parent's level or a stronger one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Isolation {
    /// A plain host process tree.
    Unsandboxed = 0,
    /// A macOS seatbelt profile around a host process tree.
    Seatbelt = 1,
    /// A container or VM.
    Container = 2,
}

/// An HTTP method an egress entry may allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `HEAD`
    Head,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
}

impl Method {
    /// Every method, which is what an egress entry without a method list allows.
    pub const ALL: [Method; 6] = [
        Method::Get,
        Method::Head,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
    ];
}

/// Mount access. `Rw` implies `Ro`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MountMode {
    /// Read only.
    Ro,
    /// Read and write.
    Rw,
}

/// A workload identity, `spiffe://<trust domain>/<path>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpiffeId(String);

impl SpiffeId {
    /// Accepts `spiffe://` followed by a non-empty trust domain.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let rest = id.strip_prefix("spiffe://")?;
        let domain = rest.split('/').next().unwrap_or_default();
        (!domain.is_empty()).then(|| Self(id.to_string()))
    }

    /// The identity as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A name the credential broker resolves, such as `broker:github/test-repo`. It is never a value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// Accepts `broker:` followed by a non-empty name, so a pasted token cannot pass as a reference.
    #[must_use]
    pub fn parse(reference: &str) -> Option<Self> {
        let name = reference.strip_prefix("broker:")?;
        (!name.is_empty()).then(|| Self(reference.to_string()))
    }

    /// The reference as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One allowed egress destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Egress {
    /// Exact host name.
    pub host: String,
    /// Allowed methods; `None` allows every method.
    pub methods: Option<BTreeSet<Method>>,
    /// Allowed path prefix, matched by whole segments; `None` means `/`.
    pub path_prefix: Option<String>,
}

impl Egress {
    /// The allowed methods with the default filled in.
    #[must_use]
    pub fn methods(&self) -> BTreeSet<Method> {
        self.methods
            .clone()
            .unwrap_or_else(|| Method::ALL.into_iter().collect())
    }

    /// The allowed path prefix with the default filled in.
    #[must_use]
    pub fn prefix(&self) -> &str {
        self.path_prefix.as_deref().unwrap_or("/")
    }
}

/// One allowed mount: a mount point, its mode, and the subtree of it the holder may touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// The mount point.
    pub path: String,
    /// The strongest mode allowed.
    pub mode: MountMode,
    /// The subtree allowed, matched by whole segments.
    pub scope: String,
}

/// What a workload may do, on every axis the attenuation rules narrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The weakest isolation the holder may spawn a child at.
    pub isolation: Isolation,
    /// Egress allowlist.
    pub egress: Vec<Egress>,
    /// Broker references the holder may ask the broker to use for it.
    pub credentials: Vec<CredentialRef>,
    /// Mounts.
    pub mounts: Vec<Mount>,
}

/// One action a workload asks `agentd` to authorize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// An outbound HTTP request.
    Egress {
        /// Host name.
        host: String,
        /// Method.
        method: Method,
        /// Request path.
        path: String,
    },
    /// Use of a broker credential.
    Credential {
        /// The reference.
        reference: CredentialRef,
    },
    /// Access below a mount.
    Mount {
        /// The mount point.
        path: String,
        /// The mode asked for.
        mode: MountMode,
        /// The path below the mount point.
        subpath: String,
    },
    /// Creation of a child workload at an isolation level.
    Spawn {
        /// The child's isolation.
        isolation: Isolation,
    },
}

/// Why a request was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    /// A path is not absolute, or has an empty, `.`, or `..` segment.
    MalformedPath,
    /// No egress entry names the host.
    UnknownHost,
    /// The reference is not in the grant.
    UnknownCredential,
    /// No mount entry has the mount point.
    UnknownMount,
    /// The host or mount is known, but no entry covers the method, mode, or path.
    NotCovered,
    /// The spawn asks for weaker isolation than the grant allows.
    WeakerIsolation,
}

/// The answer of [`within`] for one request against one grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The grant covers the request.
    Allow,
    /// The grant does not cover the request.
    Deny(Denial),
}

impl Decision {
    /// Whether this is [`Decision::Allow`].
    #[must_use]
    pub fn is_allow(self) -> bool {
        self == Decision::Allow
    }
}

/// Splits an absolute path into segments, ignoring one trailing slash. `None` for a path that is
/// not absolute, has an empty, `.`, or `..` segment, or contains `%`, so traversal fails closed.
/// Callers pass decoded paths; a percent escape is refused rather than decoded here.
fn segments(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    if rest.starts_with('/') || rest.contains('%') {
        return None;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let segs: Vec<&str> = rest.split('/').collect();
    segs.iter()
        .all(|s| !s.is_empty() && *s != "." && *s != "..")
        .then_some(segs)
}

/// Whether `path` is `prefix` or below it, by whole segments: `/workspace/src-evil/x` is not
/// below `/workspace/src/`.
fn under(path: &str, prefix: &str) -> bool {
    match (segments(path), segments(prefix)) {
        (Some(p), Some(q)) => p.starts_with(&q),
        _ => false,
    }
}

fn same_path(a: &str, b: &str) -> bool {
    segments(a).is_some_and(|a| Some(a) == segments(b))
}

/// The prefix whose subtree is the overlap of both, if they overlap.
fn narrower<'a>(a: &'a str, b: &'a str) -> Option<&'a str> {
    if under(a, b) {
        Some(a)
    } else if under(b, a) {
        Some(b)
    } else {
        None
    }
}

fn egress_covers(outer: &Egress, inner: &Egress) -> bool {
    outer.host == inner.host
        && inner.methods().is_subset(&outer.methods())
        && under(inner.prefix(), outer.prefix())
}

fn mount_covers(outer: &Mount, inner: &Mount) -> bool {
    same_path(&outer.path, &inner.path)
        && inner.mode <= outer.mode
        && under(&inner.scope, &outer.scope)
}

/// `resource_within_scope`: whether `grant` covers `request`. Unknown hosts, mounts, and
/// references, malformed paths, and a mount subpath outside its mount point are denied.
#[must_use]
pub fn within(grant: &Grant, request: &Request) -> Decision {
    let covered = |known: bool, covered: bool, unknown: Denial| match (known, covered) {
        (_, true) => Decision::Allow,
        (false, false) => Decision::Deny(unknown),
        (true, false) => Decision::Deny(Denial::NotCovered),
    };
    match request {
        Request::Egress { host, method, path } => {
            if segments(path).is_none() {
                return Decision::Deny(Denial::MalformedPath);
            }
            let entries = || grant.egress.iter().filter(|e| e.host == *host);
            covered(
                entries().next().is_some(),
                entries().any(|e| e.methods().contains(method) && under(path, e.prefix())),
                Denial::UnknownHost,
            )
        }
        Request::Credential { reference } => {
            if grant.credentials.contains(reference) {
                Decision::Allow
            } else {
                Decision::Deny(Denial::UnknownCredential)
            }
        }
        Request::Mount {
            path,
            mode,
            subpath,
        } => {
            if segments(path).is_none() || segments(subpath).is_none() {
                return Decision::Deny(Denial::MalformedPath);
            }
            let entries = || grant.mounts.iter().filter(|m| same_path(&m.path, path));
            covered(
                entries().next().is_some(),
                under(subpath, path)
                    && entries().any(|m| *mode <= m.mode && under(subpath, &m.scope)),
                Denial::UnknownMount,
            )
        }
        Request::Spawn { isolation } => {
            if *isolation >= grant.isolation {
                Decision::Allow
            } else {
                Decision::Deny(Denial::WeakerIsolation)
            }
        }
    }
}

/// Whether `child` is a subset of `parent` on every axis: its isolation is no weaker, and each of
/// its egress entries, credentials, and mounts is covered by one entry of the parent's.
#[must_use]
pub fn narrows(child: &Grant, parent: &Grant) -> bool {
    child.isolation >= parent.isolation
        && child
            .egress
            .iter()
            .all(|c| parent.egress.iter().any(|p| egress_covers(p, c)))
        && child
            .credentials
            .iter()
            .all(|c| parent.credentials.contains(c))
        && child
            .mounts
            .iter()
            .all(|c| parent.mounts.iter().any(|p| mount_covers(p, c)))
}

/// One thing [`intersect`] removed from, or narrowed in, a requested grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The requested isolation was weaker than the parent's and was raised to it.
    IsolationRaised {
        /// What the request asked for.
        requested: Isolation,
        /// What the child got.
        granted: Isolation,
    },
    /// No parent entry overlaps the requested egress entry.
    EgressRemoved {
        /// The requested host.
        host: String,
    },
    /// The requested egress entry kept fewer methods or a narrower path prefix.
    EgressNarrowed {
        /// The requested host.
        host: String,
    },
    /// The parent does not hold the reference.
    CredentialRemoved {
        /// The requested reference.
        reference: CredentialRef,
    },
    /// No parent mount overlaps the requested mount.
    MountRemoved {
        /// The requested mount point.
        path: String,
    },
    /// The requested mount asked for `rw` and the parent allows only `ro`.
    MountDowngraded {
        /// The requested mount point.
        path: String,
    },
    /// The requested mount's scope was narrowed to the parent's.
    MountNarrowed {
        /// The requested mount point.
        path: String,
    },
}

fn egress_meet(parent: &Egress, requested: &Egress) -> Option<Egress> {
    if parent.host != requested.host {
        return None;
    }
    let methods: BTreeSet<Method> = requested
        .methods()
        .intersection(&parent.methods())
        .copied()
        .collect();
    let prefix = narrower(requested.prefix(), parent.prefix())?;
    (!methods.is_empty()).then(|| Egress {
        host: requested.host.clone(),
        methods: Some(methods),
        path_prefix: Some(prefix.to_string()),
    })
}

fn mount_meet(parent: &Mount, requested: &Mount) -> Option<Mount> {
    if !same_path(&parent.path, &requested.path) {
        return None;
    }
    let scope = narrower(&requested.scope, &parent.scope)?;
    Some(Mount {
        path: requested.path.clone(),
        mode: requested.mode.min(parent.mode),
        scope: scope.to_string(),
    })
}

/// The child grant: `parent` ∩ `requested`, with everything the intersection removed or narrowed.
///
/// A requested entry that one parent entry covers is kept as asked. Any other requested entry is
/// replaced by its overlap with each parent entry, and reported in `dropped`, so a spawn can
/// refuse instead of silently narrowing. `dropped` is empty exactly when [`narrows`] holds for
/// `requested` against `parent`.
#[must_use]
pub fn intersect(parent: &Grant, requested: &Grant) -> (Grant, Vec<Dropped>) {
    let mut dropped = Vec::new();
    let isolation = requested.isolation.max(parent.isolation);
    if isolation != requested.isolation {
        dropped.push(Dropped::IsolationRaised {
            requested: requested.isolation,
            granted: isolation,
        });
    }

    let mut egress = Vec::new();
    for r in &requested.egress {
        if parent.egress.iter().any(|p| egress_covers(p, r)) {
            egress.push(r.clone());
            continue;
        }
        let meets: Vec<Egress> = parent
            .egress
            .iter()
            .filter_map(|p| egress_meet(p, r))
            .collect();
        let host = r.host.clone();
        dropped.push(if meets.is_empty() {
            Dropped::EgressRemoved { host }
        } else {
            Dropped::EgressNarrowed { host }
        });
        egress.extend(meets);
    }

    let mut credentials = Vec::new();
    for r in &requested.credentials {
        if parent.credentials.contains(r) {
            credentials.push(r.clone());
        } else {
            dropped.push(Dropped::CredentialRemoved {
                reference: r.clone(),
            });
        }
    }

    let mut mounts = Vec::new();
    for r in &requested.mounts {
        if parent.mounts.iter().any(|p| mount_covers(p, r)) {
            mounts.push(r.clone());
            continue;
        }
        let meets: Vec<Mount> = parent
            .mounts
            .iter()
            .filter_map(|p| mount_meet(p, r))
            .collect();
        let path = r.path.clone();
        if meets.is_empty() {
            dropped.push(Dropped::MountRemoved { path });
        } else {
            // Each overlap either lost `rw` or narrowed the scope, so at least one is reported.
            if meets.iter().all(|m| m.mode < r.mode) {
                dropped.push(Dropped::MountDowngraded { path: path.clone() });
            }
            if meets.iter().any(|m| !under(&r.scope, &m.scope)) {
                dropped.push(Dropped::MountNarrowed { path });
            }
        }
        mounts.extend(meets);
    }

    let child = Grant {
        isolation,
        egress,
        credentials,
        mounts,
    };
    (child, dropped)
}

/// A credential grant the broker enforces: the holder may have the broker act with the
/// reference. It carries the holder's identity and a reference, never a secret value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerGrant {
    /// The workload the broker acts for.
    pub holder: SpiffeId,
    /// The reference it may use.
    pub reference: CredentialRef,
}

/// The broker policy grants for a workload's row, each scoped to that workload's identity.
#[must_use]
pub fn broker_grants(holder: &SpiffeId, grant: &Grant) -> Vec<BrokerGrant> {
    grant
        .credentials
        .iter()
        .map(|reference| BrokerGrant {
            holder: holder.clone(),
            reference: reference.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
