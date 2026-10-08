//! Unit tests port the 22 request cases and the widening attempt from RFC-38 run 20's
//! `grant-set.json`; properties cover each attenuation invariant.

use super::{
    BrokerGrant, CredentialRef, Decision, Denial, Dropped, Egress, Grant, Isolation, Method, Mount,
    MountMode, Request, SpiffeId, broker_grants, intersect, narrows, within,
};
use proptest::prelude::*;
use std::collections::BTreeSet;

pub(super) fn id(s: &str) -> SpiffeId {
    SpiffeId::parse(s).expect("test SPIFFE ID")
}

pub(super) fn cred(s: &str) -> CredentialRef {
    CredentialRef::parse(s).expect("test reference")
}

fn egress(host: &str, methods: Option<&[Method]>, prefix: Option<&str>) -> Egress {
    Egress {
        host: host.to_string(),
        methods: methods.map(|m| m.iter().copied().collect()),
        path_prefix: prefix.map(str::to_string),
    }
}

fn mount(path: &str, mode: MountMode, scope: &str) -> Mount {
    Mount {
        path: path.to_string(),
        mode,
        scope: scope.to_string(),
    }
}

const GH: &str = "api.github.com";
pub(super) const PARENT_ID: &str = "spiffe://air.local/workspace/ws-7f3a/session/s1";
pub(super) const CHILD_ID: &str = "spiffe://air.local/workspace/ws-7f3a-c1/session/s1";

pub(super) fn parent() -> Grant {
    use Method::{Get, Patch, Post};
    Grant {
        isolation: Isolation::Container,
        egress: vec![
            egress(
                "api.github.com",
                Some(&[Get, Post, Patch]),
                Some("/repos/tbhb-dev/"),
            ),
            egress("registry.npmjs.org", Some(&[Get]), None),
            egress("index.crates.io", None, None),
            egress("api.anthropic.com", Some(&[Post]), Some("/v1/")),
        ],
        credentials: vec![
            cred("broker:github/tbhb-dev/contents-write"),
            cred("broker:anthropic/workspace-key"),
            cred("broker:npm/read-token"),
        ],
        mounts: vec![
            mount("/workspace/", MountMode::Rw, "/workspace/"),
            mount("/cache/cargo/", MountMode::Rw, "/cache/cargo/"),
            mount("/reference/", MountMode::Ro, "/reference/"),
        ],
    }
}

pub(super) fn child() -> Grant {
    Grant {
        isolation: Isolation::Container,
        egress: vec![
            egress(
                "api.github.com",
                Some(&[Method::Get]),
                Some("/repos/tbhb-dev/agent-orchestration-poc/"),
            ),
            egress("index.crates.io", None, None),
        ],
        credentials: vec![cred("broker:github/tbhb-dev/contents-write")],
        mounts: vec![
            mount("/workspace/", MountMode::Ro, "/workspace/src/"),
            mount("/cache/cargo/", MountMode::Rw, "/cache/cargo/"),
        ],
    }
}

pub(super) fn widen() -> Grant {
    Grant {
        isolation: Isolation::Unsandboxed,
        egress: vec![
            egress(
                "api.github.com",
                Some(&[Method::Get, Method::Delete]),
                Some("/"),
            ),
            egress("exfil.example.net", None, None),
        ],
        credentials: vec![
            cred("broker:github/tbhb-dev/contents-write"),
            cred("broker:aws/admin"),
        ],
        mounts: vec![
            mount("/reference/", MountMode::Rw, "/reference/"),
            mount("/", MountMode::Rw, "/"),
        ],
    }
}

fn get(host: &str, path: &str) -> Request {
    http(host, Method::Get, path)
}

fn http(host: &str, method: Method, path: &str) -> Request {
    Request::Egress {
        host: host.to_string(),
        method,
        path: path.to_string(),
    }
}

pub(super) fn use_cred(s: &str) -> Request {
    Request::Credential { reference: cred(s) }
}

fn at(path: &str, mode: MountMode, subpath: &str) -> Request {
    Request::Mount {
        path: path.to_string(),
        mode,
        subpath: subpath.to_string(),
    }
}

fn spawn(isolation: Isolation) -> Request {
    Request::Spawn { isolation }
}

/// The cases the widening attempt targets; the widened child must deny each.
const WIDEN_TARGETS: [&str; 6] = [
    "exfil-host",
    "cred-aws",
    "mount-ref-rw",
    "mount-host-root",
    "spawn-seatbelt",
    "spawn-unsandboxed",
];

pub(super) fn case(name: &str) -> Request {
    let found = cases().into_iter().find(|c| c.0 == name);
    found.expect("known case").1
}

/// One case against the parent grant and the child grant computed by `intersect`.
fn run_case(request: &Request, want_parent: bool, want_child: bool) {
    let (computed, _) = intersect(&parent(), &child());
    assert_eq!(within(&parent(), request).is_allow(), want_parent, "parent");
    assert_eq!(within(&computed, request).is_allow(), want_child, "child");
}

/// Defines `cases()`, the request cases with their parent and child answers, and one test each.
macro_rules! cases {
    ($($test:ident: $name:literal, $request:expr, $parent:literal, $child:literal;)*) => {
        pub(super) fn cases() -> Vec<(&'static str, Request, bool, bool)> {
            vec![$(($name, $request, $parent, $child)),*]
        }
        $(#[test] fn $test() { run_case(&$request, $parent, $child); })*
    };
}

// The 22 cases from `grant-set.json`: test, name, request, parent answer, child answer.
cases! {
    case_gh_get_poc: "gh-get-poc", get(GH, "/repos/tbhb-dev/agent-orchestration-poc/pulls"), true, true;
    case_gh_post_poc: "gh-post-poc", http(GH, Method::Post, "/repos/tbhb-dev/agent-orchestration-poc/pulls"), true, false;
    case_gh_get_other_repo: "gh-get-other-repo", get(GH, "/repos/tbhb-dev/other/pulls"), true, false;
    case_gh_get_other_org: "gh-get-other-org", get(GH, "/repos/someone-else/x/pulls"), false, false;
    case_gh_prefix_confusion: "gh-prefix-confusion", get(GH, "/repos/tbhb-dev/agent-orchestration-poc.evil/x"), true, false;
    case_crates_get: "crates-get", get("index.crates.io", "/se/rd/serde"), true, true;
    case_npm_get: "npm-get", get("registry.npmjs.org", "/left-pad"), true, false;
    case_anthropic_post: "anthropic-post", http("api.anthropic.com", Method::Post, "/v1/messages"), true, false;
    case_exfil_host: "exfil-host", http("exfil.example.net", Method::Post, "/"), false, false;
    case_cred_github: "cred-github", use_cred("broker:github/tbhb-dev/contents-write"), true, true;
    case_cred_anthropic: "cred-anthropic", use_cred("broker:anthropic/workspace-key"), true, false;
    case_cred_aws: "cred-aws", use_cred("broker:aws/admin"), false, false;
    case_mount_ws_rw_root: "mount-ws-rw-root", at("/workspace/", MountMode::Rw, "/workspace/README.md"), true, false;
    case_mount_ws_ro_src: "mount-ws-ro-src", at("/workspace/", MountMode::Ro, "/workspace/src/main.rs"), true, true;
    case_mount_ws_ro_root: "mount-ws-ro-root", at("/workspace/", MountMode::Ro, "/workspace/.env"), true, false;
    case_mount_ws_src_prefix_confusion: "mount-ws-src-prefix-confusion", at("/workspace/", MountMode::Ro, "/workspace/src-evil/x"), true, false;
    case_mount_ref_ro: "mount-ref-ro", at("/reference/", MountMode::Ro, "/reference/spec.md"), true, false;
    case_mount_ref_rw: "mount-ref-rw", at("/reference/", MountMode::Rw, "/reference/spec.md"), false, false;
    case_mount_host_root: "mount-host-root", at("/", MountMode::Rw, "/etc/passwd"), false, false;
    case_spawn_container: "spawn-container", spawn(Isolation::Container), true, true;
    case_spawn_seatbelt: "spawn-seatbelt", spawn(Isolation::Seatbelt), false, false;
    case_spawn_unsandboxed: "spawn-unsandboxed", spawn(Isolation::Unsandboxed), false, false;
}

#[test]
fn there_are_22_distinct_cases() {
    let names: BTreeSet<&str> = cases().into_iter().map(|c| c.0).collect();
    assert_eq!(names.len(), 22);
}

#[test]
fn computed_child_equals_the_requested_child() {
    assert_eq!(intersect(&parent(), &child()), (child(), Vec::new()));
}

#[test]
fn widening_attempt_reports_everything_dropped() {
    let (_, dropped) = intersect(&parent(), &widen());
    let path = |p: &str| p.to_string();
    assert_eq!(
        dropped,
        vec![
            Dropped::IsolationRaised {
                requested: Isolation::Unsandboxed,
                granted: Isolation::Container,
            },
            Dropped::EgressNarrowed {
                host: "api.github.com".into()
            },
            Dropped::EgressRemoved {
                host: "exfil.example.net".into()
            },
            Dropped::CredentialRemoved {
                reference: cred("broker:aws/admin")
            },
            Dropped::MountDowngraded {
                path: path("/reference/")
            },
            Dropped::MountRemoved { path: path("/") },
        ]
    );
}

#[test]
fn widened_child_denies_every_widen_target() {
    let (widened, _) = intersect(&parent(), &widen());
    for name in WIDEN_TARGETS {
        assert!(!within(&widened, &case(name)).is_allow(), "{name}");
    }
}

#[test]
fn widened_child_allows_nothing_the_parent_denies() {
    let (widened, _) = intersect(&parent(), &widen());
    for (name, request, want_parent, _) in cases() {
        assert!(
            want_parent || !within(&widened, &request).is_allow(),
            "{name}"
        );
    }
}

#[test]
fn credential_outside_the_parent_fails() {
    let mut requested = child();
    requested.credentials.push(cred("broker:aws/admin"));
    let (grant, dropped) = intersect(&parent(), &requested);
    assert_eq!(
        dropped,
        vec![Dropped::CredentialRemoved {
            reference: cred("broker:aws/admin")
        }]
    );
    let request = use_cred("broker:aws/admin");
    assert_eq!(
        within(&grant, &request),
        Decision::Deny(Denial::UnknownCredential)
    );
}

#[test]
fn broker_grants_are_scoped_to_the_child_identity() {
    let (grant, _) = intersect(&parent(), &child());
    let grants = broker_grants(&id(CHILD_ID), &grant);
    assert_eq!(
        grants,
        vec![BrokerGrant {
            holder: id(CHILD_ID),
            reference: cred("broker:github/tbhb-dev/contents-write"),
        }]
    );
}

#[test]
fn a_secret_value_is_not_a_reference() {
    assert_eq!(CredentialRef::parse("ghp_0123456789abcdef"), None);
    assert_eq!(CredentialRef::parse("broker:"), None);
    assert_eq!(
        cred("broker:github/test-repo").as_str(),
        "broker:github/test-repo"
    );
}

#[test]
fn spiffe_ids_need_the_scheme_and_a_trust_domain() {
    assert_eq!(SpiffeId::parse("https://air.local/x"), None);
    assert_eq!(SpiffeId::parse("spiffe:///x"), None);
    assert_eq!(id(CHILD_ID).as_str(), CHILD_ID);
}

#[test]
fn traversal_and_empty_segments_fail_closed() {
    let g = parent();
    for path in [
        "/repos/tbhb-dev/../someone-else/x",
        "/repos/tbhb-dev/./x",
        "/repos//tbhb-dev/x",
        "repos/tbhb-dev/x",
    ] {
        assert_eq!(
            within(&g, &get("api.github.com", path)),
            Decision::Deny(Denial::MalformedPath),
            "{path}"
        );
    }
    assert_eq!(
        within(
            &g,
            &at("/workspace/", MountMode::Ro, "/workspace/src/../../etc")
        ),
        Decision::Deny(Denial::MalformedPath)
    );
}

#[test]
fn a_prefix_without_a_trailing_slash_still_matches_whole_segments() {
    let mut g = child();
    g.mounts[0].scope = "/workspace/src".into();
    let ok = at("/workspace/", MountMode::Ro, "/workspace/src/main.rs");
    let evil = at("/workspace/", MountMode::Ro, "/workspace/src-evil/x");
    assert!(within(&g, &ok).is_allow());
    assert_eq!(within(&g, &evil), Decision::Deny(Denial::NotCovered));
}

#[test]
fn unknown_mount_and_not_covered_are_told_apart() {
    let g = child();
    assert_eq!(
        within(&g, &at("/reference/", MountMode::Ro, "/reference/a")),
        Decision::Deny(Denial::UnknownMount)
    );
    assert_eq!(
        within(&g, &at("/workspace/", MountMode::Rw, "/workspace/src/a")),
        Decision::Deny(Denial::NotCovered)
    );
}

#[test]
fn a_scope_narrowed_by_the_parent_is_reported() {
    let mut requested = child();
    requested.mounts = vec![mount("/workspace/", MountMode::Ro, "/")];
    let (grant, dropped) = intersect(&parent(), &requested);
    assert_eq!(
        grant.mounts,
        vec![mount("/workspace/", MountMode::Ro, "/workspace/")]
    );
    assert_eq!(
        dropped,
        vec![Dropped::MountNarrowed {
            path: "/workspace/".into()
        }]
    );
}

// Property inputs draw from small universes so that overlaps, prefix confusion, and traversal
// come up often.

const HOSTS: [&str; 3] = ["a.test", "b.test", "c.test"];
const PREFIXES: [&str; 7] = ["/", "/x/", "/x/y/", "/x/y-evil/", "/x/y/z/", "/q", "/x/y"];
const PATHS: [&str; 8] = [
    "/",
    "/x/",
    "/x/y/f",
    "/x/y-evil/f",
    "/x/y/z/f",
    "/q/f",
    "/x/y/../f",
    "/x//y/f",
];
const REFS: [&str; 4] = ["broker:a", "broker:b", "broker:c", "broker:d"];
const MOUNT_POINTS: [&str; 3] = ["/w/", "/c/", "/"];
const SCOPES: [&str; 6] = ["/", "/w/", "/w/s/", "/w/s-evil/", "/w/s", "/c/"];
const SUBPATHS: [&str; 7] = [
    "/w/f",
    "/w/s/f",
    "/w/s-evil/f",
    "/w/s/../f",
    "/c/f",
    "/etc/passwd",
    "/",
];

fn isolation() -> impl Strategy<Value = Isolation> {
    prop::sample::select(vec![
        Isolation::Unsandboxed,
        Isolation::Seatbelt,
        Isolation::Container,
    ])
}

fn method() -> impl Strategy<Value = Method> {
    prop::sample::select(Method::ALL.to_vec())
}

fn mode() -> impl Strategy<Value = MountMode> {
    prop::sample::select(vec![MountMode::Ro, MountMode::Rw])
}

fn prefix() -> impl Strategy<Value = String> {
    prop::sample::select(PREFIXES.to_vec()).prop_map(str::to_string)
}

fn path() -> impl Strategy<Value = String> {
    prop::sample::select(PATHS.to_vec()).prop_map(str::to_string)
}

fn host() -> impl Strategy<Value = String> {
    prop::sample::select(HOSTS.to_vec()).prop_map(str::to_string)
}

fn scope() -> impl Strategy<Value = String> {
    prop::sample::select(SCOPES.to_vec()).prop_map(str::to_string)
}

fn subpath() -> impl Strategy<Value = String> {
    prop::sample::select(SUBPATHS.to_vec()).prop_map(str::to_string)
}

fn mount_point() -> impl Strategy<Value = String> {
    prop::sample::select(MOUNT_POINTS.to_vec()).prop_map(str::to_string)
}

fn reference() -> impl Strategy<Value = CredentialRef> {
    prop::sample::select(REFS.to_vec()).prop_map(cred)
}

pub(super) fn grant() -> impl Strategy<Value = Grant> {
    let egress = (
        host(),
        prop::option::of(prop::collection::btree_set(method(), 0..4)),
        prop::option::of(prefix()),
    )
        .prop_map(|(host, methods, path_prefix)| Egress {
            host,
            methods,
            path_prefix,
        });
    let mount = (mount_point(), mode(), scope()).prop_map(|(path, mode, scope)| Mount {
        path,
        mode,
        scope,
    });
    (
        isolation(),
        prop::collection::vec(egress, 0..4),
        prop::collection::vec(reference(), 0..3),
        prop::collection::vec(mount, 0..4),
    )
        .prop_map(|(isolation, egress, credentials, mounts)| Grant {
            isolation,
            egress,
            credentials,
            mounts,
        })
}

pub(super) fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        (host(), method(), path()).prop_map(|(host, method, path)| Request::Egress {
            host,
            method,
            path
        }),
        reference().prop_map(|reference| Request::Credential { reference }),
        (mount_point(), mode(), subpath()).prop_map(|(path, mode, subpath)| Request::Mount {
            path,
            mode,
            subpath
        }),
        isolation().prop_map(|isolation| Request::Spawn { isolation }),
    ]
}

proptest! {
    #[test]
    fn child_is_a_subset_of_the_parent_on_every_axis(
        p in grant(), r in grant(), req in request(),
    ) {
        let (c, _) = intersect(&p, &r);
        prop_assert!(narrows(&c, &p));
        if within(&c, &req).is_allow() {
            prop_assert!(within(&p, &req).is_allow());
        }
    }

    #[test]
    fn child_allows_exactly_what_both_parent_and_request_allow(
        p in grant(), r in grant(), req in request(),
    ) {
        let (c, _) = intersect(&p, &r);
        let both = within(&p, &req).is_allow() && within(&r, &req).is_allow();
        prop_assert_eq!(within(&c, &req).is_allow(), both);
    }

    #[test]
    fn intersect_is_idempotent(p in grant(), r in grant()) {
        let (c, _) = intersect(&p, &r);
        prop_assert_eq!(intersect(&p, &c), (c, Vec::new()));
    }

    #[test]
    fn isolation_never_decreases(p in grant(), r in grant()) {
        let (c, _) = intersect(&p, &r);
        prop_assert!(c.isolation >= p.isolation);
        prop_assert!(c.isolation >= r.isolation);
    }

    #[test]
    fn dropped_is_empty_exactly_when_the_request_is_within_the_parent(
        p in grant(), r in grant(),
    ) {
        let (_, dropped) = intersect(&p, &r);
        prop_assert_eq!(dropped.is_empty(), narrows(&r, &p));
    }

    #[test]
    fn broker_grants_name_only_the_holder_and_parent_held_references(
        p in grant(), r in grant(),
    ) {
        let (c, _) = intersect(&p, &r);
        let holder = id(CHILD_ID);
        for g in broker_grants(&holder, &c) {
            prop_assert_eq!(&g.holder, &holder);
            prop_assert!(p.credentials.contains(&g.reference));
        }
    }
}
