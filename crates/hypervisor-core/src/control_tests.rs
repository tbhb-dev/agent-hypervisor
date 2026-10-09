use super::*;
use crate::channel::SessionKind;
use proptest::prelude::*;

const OPERATOR: Operator = Operator { uid: 501, gid: 20 };

fn id(local: &str) -> StableId {
    StableId {
        host: "h".into(),
        local: local.into(),
    }
}

fn spawned(local: &str) -> Change {
    Change::SessionSpawned {
        workload: id("w"),
        session: id(local),
        socket: PathBuf::from(format!("/r/{local}.s")),
    }
}

fn kill(request_id: Option<&str>, local: &str) -> Forwarded {
    Forwarded {
        operator: OPERATOR,
        request_id: request_id.map(str::to_owned),
        request: Request::KillSession {
            workload: id("w"),
            session: id(local),
        },
    }
}

fn filled(count: usize) -> (EventLog, Vec<u8>) {
    let mut log = EventLog::new("h");
    let mut bytes = Vec::new();
    for index in 0..count {
        let name = format!("s{index}");
        bytes.extend(
            log.append(&format!("r{index}"), "f", OPERATOR, spawned(&name))
                .1,
        );
    }
    (log, bytes)
}

#[test]
fn client_cannot_claim_an_operator() {
    let claimed = r#"{"request_id":null,"request":"list_workloads","operator":{"uid":0,"gid":0}}"#;
    assert!(serde_json::from_str::<ClientRequest>(claimed).is_err());
    let client: ClientRequest =
        serde_json::from_str(r#"{"request_id":"a","request":"list_workloads"}"#).unwrap();
    let forwarded = forward(client, OPERATOR);
    assert_eq!(
        (forwarded.operator, forwarded.request_id.as_deref()),
        (OPERATOR, Some("a"))
    );
}

#[test]
fn reads_need_no_request_id_and_mutations_do() {
    let log = EventLog::new("h");
    let read = Forwarded {
        operator: OPERATOR,
        request_id: None,
        request: Request::ListWorkloads,
    };
    assert_eq!(plan(&log, &read, "f"), Plan::Query);
    for missing in [None, Some("")] {
        assert_eq!(
            plan(&log, &kill(missing, "s"), "f"),
            Plan::Reply(Response::Error(ControlError::MissingRequestId))
        );
    }
    assert!(
        Request::SpawnSession {
            workload: id("w"),
            session: id("s"),
            spec: WireSpawnSpec {
                command: "/bin/sh".into(),
                args: vec![],
                env: vec![],
                cwd: None,
                user: None,
                kind: SessionKind::Shell,
            },
            size: WireSize { cols: 80, rows: 24 },
        }
        .mutates()
    );
}

#[test]
fn watch_rejects_foreign_and_future_cursors() {
    let (log, _) = filled(2);
    let watch = |after| Forwarded {
        operator: OPERATOR,
        request_id: None,
        request: Request::Watch { after: Some(after) },
    };
    let foreign = Cursor {
        host: "other".into(),
        seq: 1,
    };
    let ahead = Cursor {
        host: "h".into(),
        seq: 3,
    };
    assert_eq!(
        plan(&log, &watch(foreign), "f"),
        Plan::Reply(Response::Error(ControlError::ForeignCursor))
    );
    assert_eq!(
        plan(&log, &watch(ahead), "f"),
        Plan::Reply(Response::Error(ControlError::CursorAhead))
    );
    assert_eq!(plan(&log, &watch(log.head()), "f"), Plan::Stream(&[]));
}

#[test]
fn restore_refuses_gaps_foreign_hosts_and_repeated_ids() {
    let (_, bytes) = filled(3);
    let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    let gap = [lines[0], lines[2]].concat();
    assert_eq!(EventLog::restore("h", &gap), Err(CorruptLine(1)));
    assert_eq!(EventLog::restore("other", &bytes), Err(CorruptLine(0)));
    assert_eq!(EventLog::restore("h", b"{not json}\n"), Err(CorruptLine(0)));
    let mut log = EventLog::new("h");
    let mut repeated = log.append("r", "f", OPERATOR, spawned("a")).1;
    let mut second = EventLog::new("h");
    let _ = second.append("x", "f", OPERATOR, spawned("a"));
    repeated.extend(second.append("r", "f", OPERATOR, spawned("b")).1);
    assert_eq!(EventLog::restore("h", &repeated), Err(CorruptLine(1)));
}

#[test]
fn detail_joins_the_live_list_with_the_logged_spawn() {
    let (log, _) = filled(1);
    let detail = log
        .session_detail(&id("w"), &id("s0"), &[id("s0")])
        .unwrap();
    assert_eq!(
        (detail.live, detail.socket, detail.spawned_by),
        (true, Some(PathBuf::from("/r/s0.s")), Some(OPERATOR))
    );
    assert!(!log.session_detail(&id("w"), &id("s0"), &[]).unwrap().live);
    let unlogged = log.session_detail(&id("w"), &id("x"), &[id("x")]).unwrap();
    assert_eq!((unlogged.socket, unlogged.spawned_by), (None, None));
    assert_eq!(log.session_detail(&id("w"), &id("x"), &[]), None);
}

proptest! {
    #[test]
    fn appends_number_contiguously_and_restore_round_trips(count in 0usize..20) {
        let (log, bytes) = filled(count);
        prop_assert_eq!(log.head().seq, count as u64);
        let seqs: Vec<u64> = log.after(None).unwrap().iter().map(|event| event.cursor.seq).collect();
        prop_assert_eq!(seqs, (1..=count as u64).collect::<Vec<_>>());
        prop_assert_eq!(EventLog::restore("h", &bytes), Ok((log, bytes.len())));
    }

    #[test]
    fn a_torn_append_restores_the_complete_prefix(count in 1usize..12, cut in any::<prop::sample::Index>()) {
        let (_, bytes) = filled(count);
        let cut = cut.index(bytes.len() + 1);
        let (log, valid) = EventLog::restore("h", &bytes[..cut]).unwrap();
        let complete = bytes[..cut].split(|byte| *byte == b'\n').count() - 1;
        prop_assert_eq!(log.head().seq, complete as u64);
        prop_assert_eq!(valid, bytes[..cut].iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1));
        prop_assert_eq!(filled(complete).0, log);
    }

    #[test]
    fn resuming_from_any_seen_event_yields_the_rest_once(count in 0usize..20, seen in any::<prop::sample::Index>()) {
        let (log, _) = filled(count);
        let all = log.after(None).unwrap();
        let seen = seen.index(count + 1);
        let cursor = if seen == 0 { None } else { Some(all[seen - 1].cursor.clone()) };
        let rest = log.after(cursor.as_ref()).unwrap();
        prop_assert_eq!([&all[..seen], rest].concat(), all.to_vec());
    }

    #[test]
    fn each_request_id_executes_once_and_replays_its_outcome(
        calls in prop::collection::vec((0u8..4, 0u8..2), 0..30),
    ) {
        let mut log = EventLog::new("h");
        let mut first: BTreeMap<u8, (u8, Response)> = BTreeMap::new();
        for (key, fingerprint) in calls {
            let request_id = key.to_string();
            let fingerprint_text = fingerprint.to_string();
            let request = kill(Some(&request_id), &format!("s{key}"));
            match (plan(&log, &request, &fingerprint_text), first.get(&key)) {
                (Plan::Execute { request_id: run }, None) => {
                    prop_assert_eq!(&run, &request_id);
                    let change = Change::SessionKilled { workload: id("w"), session: id(&format!("s{key}")) };
                    let (event, _) = log.append(&run, &fingerprint_text, OPERATOR, change);
                    first.insert(key, (fingerprint, outcome(&event)));
                }
                (Plan::Reply(response), Some((original, replayed))) if *original == fingerprint => {
                    prop_assert_eq!(&response, replayed);
                }
                (Plan::Reply(response), Some(_)) => {
                    prop_assert_eq!(response, Response::Error(ControlError::RequestIdReused));
                }
                (other, _) => prop_assert!(false, "unexpected plan {:?}", other),
            }
        }
        prop_assert_eq!(log.head().seq, first.len() as u64);
    }
}
