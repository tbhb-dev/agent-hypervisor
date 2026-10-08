//! Grant table tests: run 20's cases in chain and snapshot modes, its rogue row, live narrowing,
//! revocation, and the chain mode property.

use super::tests::{
    CHILD_ID, PARENT_ID, case, cases, child, cred, grant, id, parent, request, use_cred, widen,
};
use super::{
    BrokerGrant, Denial, Grant, GrantTable, Isolation, Request, Row, SpiffeId, Verdict,
    broker_grants, check, check_snapshot, spawn_row, within,
};
use proptest::prelude::*;

/// A table with the parent as a root row and the computed child row under it.
fn table() -> GrantTable {
    let mut t = GrantTable::new();
    t.insert(
        id(PARENT_ID),
        Row {
            parent: None,
            grant: parent(),
        },
    );
    let (row, dropped) = spawn_row(&t, &id(PARENT_ID), &child()).expect("parent row");
    assert!(dropped.is_empty());
    t.insert(id(CHILD_ID), row);
    t
}

#[test]
fn every_case_matches_in_chain_and_snapshot_modes() {
    let t = table();
    let allow = |v: Verdict| v == Verdict::Allow;
    for (name, request, want_parent, want_child) in cases() {
        assert_eq!(
            allow(check(&t, &id(PARENT_ID), &request)),
            want_parent,
            "{name}"
        );
        assert_eq!(
            allow(check(&t, &id(CHILD_ID), &request)),
            want_child,
            "{name}"
        );
        let snapshot = check_snapshot(&t, &id(CHILD_ID), &request);
        assert_eq!(allow(snapshot), want_child, "{name}");
    }
}

/// A row stored without intersection, standing in for an `agentd` bug.
fn rogue_table() -> (GrantTable, SpiffeId) {
    let mut t = table();
    let rogue = id("spiffe://air.local/workspace/ws-7f3a-rogue/session/s1");
    t.insert(
        rogue.clone(),
        Row {
            parent: Some(id(PARENT_ID)),
            grant: widen(),
        },
    );
    (t, rogue)
}

#[test]
fn rogue_row_widens_seven_cases_in_snapshot_mode() {
    let (t, rogue) = rogue_table();
    let widened = cases()
        .into_iter()
        .filter(|(_, request, want_parent, _)| {
            !want_parent && check_snapshot(&t, &rogue, request) == Verdict::Allow
        })
        .count();
    assert_eq!(widened, 7);
}

#[test]
fn rogue_row_widens_nothing_in_chain_mode() {
    let (t, rogue) = rogue_table();
    for (name, request, want_parent, _) in cases() {
        let allowed = check(&t, &rogue, &request) == Verdict::Allow;
        assert!(want_parent || !allowed, "{name}");
    }
}

#[test]
fn chain_mode_narrows_a_live_child_when_the_parent_loses_a_host() {
    let mut t = table();
    let request = case("gh-get-poc");
    let p = t.get_mut(&id(PARENT_ID)).expect("parent row");
    p.grant.egress.retain(|e| e.host != "api.github.com");
    assert_eq!(check_snapshot(&t, &id(CHILD_ID), &request), Verdict::Allow);
    assert_eq!(
        check(&t, &id(CHILD_ID), &request),
        Verdict::Deny {
            row: id(PARENT_ID),
            denial: Denial::UnknownHost
        }
    );
}

#[test]
fn chain_mode_denies_a_child_whose_parent_row_is_revoked() {
    let mut t = table();
    let request = case("crates-get");
    t.remove(&id(PARENT_ID));
    assert_eq!(check_snapshot(&t, &id(CHILD_ID), &request), Verdict::Allow);
    assert_eq!(
        check(&t, &id(CHILD_ID), &request),
        Verdict::Deny {
            row: id(PARENT_ID),
            denial: Denial::UnknownRow
        }
    );
}

#[test]
fn unknown_peer_is_denied() {
    let t = table();
    let stranger = id("spiffe://air.local/workspace/ws-0000/session/s1");
    let request = case("gh-get-poc");
    let denied = Verdict::Deny {
        row: stranger.clone(),
        denial: Denial::UnknownRow,
    };
    assert_eq!(check(&t, &stranger, &request), denied);
    assert_eq!(check_snapshot(&t, &stranger, &request), denied);
}

#[test]
fn looping_parent_links_are_denied() {
    let a = id("spiffe://t/a");
    let b = id("spiffe://t/b");
    let row = |parent: &SpiffeId| Row {
        parent: Some(parent.clone()),
        grant: Grant {
            isolation: Isolation::Unsandboxed,
            egress: Vec::new(),
            credentials: Vec::new(),
            mounts: Vec::new(),
        },
    };
    let t: GrantTable = [(a.clone(), row(&b)), (b.clone(), row(&a))].into();
    let request = Request::Spawn {
        isolation: Isolation::Container,
    };
    assert!(matches!(
        check(&t, &a, &request),
        Verdict::Deny {
            denial: Denial::Cycle,
            ..
        }
    ));
}

#[test]
fn spawn_row_needs_a_parent_row() {
    let missing = spawn_row(&GrantTable::new(), &id(PARENT_ID), &child());
    assert_eq!(missing, None);
}

#[test]
fn a_stored_credential_outside_the_parent_fails_in_chain_mode() {
    let mut requested = child();
    requested.credentials.push(cred("broker:aws/admin"));
    let mut t = table();
    let rogue = id("spiffe://air.local/workspace/ws-7f3a-c2/session/s1");
    t.insert(
        rogue.clone(),
        Row {
            parent: Some(id(PARENT_ID)),
            grant: requested,
        },
    );
    assert_eq!(
        check(&t, &rogue, &use_cred("broker:aws/admin")),
        Verdict::Deny {
            row: id(PARENT_ID),
            denial: Denial::UnknownCredential
        }
    );
}

#[test]
fn broker_grants_are_scoped_to_the_child_identity() {
    let grants = broker_grants(&table(), &id(CHILD_ID));
    let want = BrokerGrant {
        holder: id(CHILD_ID),
        reference: cred("broker:github/tbhb-dev/contents-write"),
    };
    assert_eq!(grants, Some(vec![want]));
}

#[test]
fn a_rogue_row_gets_no_broker_grant_its_parent_denies() {
    let (t, rogue) = rogue_table();
    // The rogue row lists broker:aws/admin; the parent does not, so chain mode drops it.
    assert!(
        t[&rogue]
            .grant
            .credentials
            .contains(&cred("broker:aws/admin"))
    );
    let want = BrokerGrant {
        holder: rogue.clone(),
        reference: cred("broker:github/tbhb-dev/contents-write"),
    };
    assert_eq!(broker_grants(&t, &rogue), Some(vec![want]));
}

#[test]
fn an_unknown_holder_gets_no_broker_grants() {
    let stranger = id("spiffe://air.local/workspace/ws-0000/session/s1");
    assert_eq!(broker_grants(&table(), &stranger), None);
}

/// A chain of rows, root first, with grants that were never intersected.
fn chain(grants: &[Grant]) -> (GrantTable, Vec<SpiffeId>) {
    let ids: Vec<SpiffeId> = (0..grants.len())
        .map(|i| id(&format!("spiffe://t/w{i}")))
        .collect();
    let table = grants
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let row = Row {
                parent: i.checked_sub(1).map(|p| ids[p].clone()),
                grant: g.clone(),
            };
            (ids[i].clone(), row)
        })
        .collect();
    (table, ids)
}

/// A chain built by real spawns from random requests, with one row then overwritten by an
/// arbitrary grant, the shape of the `agentd` bug chain mode exists to catch.
fn spawned_chain_with_a_rogue_row(
    root: &Grant,
    requests: &[Grant],
    rogue_at: usize,
    rogue: &Grant,
) -> (GrantTable, Vec<SpiffeId>) {
    let mut ids = vec![id("spiffe://t/w0")];
    let mut table = GrantTable::new();
    let root_row = Row {
        parent: None,
        grant: root.clone(),
    };
    table.insert(ids[0].clone(), root_row);
    for (i, requested) in requests.iter().enumerate() {
        let parent = ids[i].clone();
        let (row, _) = spawn_row(&table, &parent, requested).expect("parent row");
        let child = id(&format!("spiffe://t/w{}", i + 1));
        table.insert(child.clone(), row);
        ids.push(child);
    }
    let k = &ids[rogue_at % ids.len()];
    table.get_mut(k).expect("row").grant = rogue.clone();
    (table, ids)
}

proptest! {
    #[test]
    fn chain_mode_never_allows_what_any_ancestor_denies(
        grants in prop::collection::vec(grant(), 1..5), req in request(),
    ) {
        let (table, ids) = chain(&grants);
        let leaf = ids.last().expect("non-empty chain");
        let all = grants.iter().all(|g| within(g, &req).is_allow());
        prop_assert_eq!(check(&table, leaf, &req) == Verdict::Allow, all);
    }

    #[test]
    fn chain_mode_matches_every_row_in_a_spawned_chain_with_a_rogue_row(
        root in grant(),
        requests in prop::collection::vec(grant(), 1..5),
        rogue_at in any::<usize>(),
        rogue in grant(),
        req in request(),
    ) {
        let (table, ids) = spawned_chain_with_a_rogue_row(&root, &requests, rogue_at, &rogue);
        let leaf = ids.last().expect("non-empty chain");
        let all = ids.iter().all(|i| within(&table[i].grant, &req).is_allow());
        prop_assert_eq!(check(&table, leaf, &req) == Verdict::Allow, all);
    }

    #[test]
    fn broker_grants_are_exactly_the_references_every_row_holds(
        grants in prop::collection::vec(grant(), 1..5),
    ) {
        let (table, ids) = chain(&grants);
        let leaf = ids.last().expect("non-empty chain");
        let got = broker_grants(&table, leaf).expect("leaf row");
        let leaf_refs = &table[leaf].grant.credentials;
        let want: Vec<BrokerGrant> = leaf_refs
            .iter()
            .filter(|r| grants.iter().all(|g| g.credentials.contains(r)))
            .map(|r| BrokerGrant { holder: leaf.clone(), reference: r.clone() })
            .collect();
        prop_assert_eq!(got, want);
    }
}
