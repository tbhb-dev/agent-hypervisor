//! Control API values and the daemon's decisions: the per-host append-only event log,
//! resumable cursors, and idempotent requests keyed by client-supplied request IDs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::channel::{WireSize, WireSpawnSpec};
use crate::workload::StableId;

/// The local identity the proxy reads from peer credentials and attaches to each call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operator {
    pub uid: u32,
    pub gid: u32,
}

/// A control API operation. `Debug` is absent because a spawn spec can carry secrets in its env.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    ListWorkloads,
    ListSessions {
        workload: StableId,
    },
    SessionDetail {
        workload: StableId,
        session: StableId,
    },
    SpawnSession {
        workload: StableId,
        session: StableId,
        spec: WireSpawnSpec,
        size: WireSize,
    },
    KillSession {
        workload: StableId,
        session: StableId,
    },
    /// Stream every event after `after`, or the whole log when it is absent, then follow it.
    Watch {
        after: Option<Cursor>,
    },
}

impl Request {
    /// Mutations change host state, so they need a request ID and are logged.
    #[must_use]
    pub fn mutates(&self) -> bool {
        matches!(self, Self::SpawnSession { .. } | Self::KillSession { .. })
    }
}

/// What a client sends the proxy. It has no identity field, so a client cannot claim one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientRequest {
    pub request_id: Option<String>,
    pub request: Request,
}

/// What the proxy forwards to the daemon, with the operator it observed attached.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forwarded {
    pub operator: Operator,
    pub request_id: Option<String>,
    pub request: Request,
}

#[must_use]
pub fn forward(client: ClientRequest, operator: Operator) -> Forwarded {
    Forwarded {
        operator,
        request_id: client.request_id,
        request: client.request,
    }
}

/// A position in one host's log: the last event a subscriber has seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub host: String,
    pub seq: u64,
}

/// A host state change caused by a control request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    SessionSpawned {
        workload: StableId,
        session: StableId,
        socket: PathBuf,
    },
    SessionKilled {
        workload: StableId,
        session: StableId,
    },
}

/// One log entry. The fingerprint is the shell's digest of the request, never the request itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub cursor: Cursor,
    pub request_id: String,
    pub fingerprint: String,
    pub operator: Operator,
    pub change: Change,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlError {
    MissingRequestId,
    /// The request ID was already used for a different request.
    RequestIdReused,
    ForeignCursor,
    /// The cursor is past the head, so the log the client saw is gone; resync from the start.
    CursorAhead,
    UnknownSession,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub workload: StableId,
    pub session: StableId,
    pub live: bool,
    pub socket: Option<PathBuf>,
    pub spawned_by: Option<Operator>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Workloads(Vec<StableId>),
    Sessions(Vec<StableId>),
    Detail(SessionDetail),
    Spawned { socket: PathBuf, cursor: Cursor },
    Killed { cursor: Cursor },
    Event(Event),
    Error(ControlError),
}

/// The response a logged change produces, both when executed and when replayed.
#[must_use]
pub fn outcome(event: &Event) -> Response {
    let cursor = event.cursor.clone();
    match &event.change {
        Change::SessionSpawned { socket, .. } => Response::Spawned {
            socket: socket.clone(),
            cursor,
        },
        Change::SessionKilled { .. } => Response::Killed { cursor },
    }
}

/// A corrupt log is refused rather than repaired, because a gap would break every cursor.
#[derive(Debug, PartialEq, Eq)]
pub struct CorruptLine(pub usize);

/// One host's append-only log, with an index from request ID to entry.
#[derive(Debug, PartialEq, Eq)]
pub struct EventLog {
    host: String,
    events: Vec<Event>,
    by_request: BTreeMap<String, usize>,
}

impl EventLog {
    #[must_use]
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            events: Vec::new(),
            by_request: BTreeMap::new(),
        }
    }

    /// Rebuild a log from its newline-delimited file, returning the length of the valid prefix.
    /// A final line without its newline is a torn append; the shell truncates it away.
    ///
    /// # Errors
    /// A complete line that does not parse, names another host, skips a sequence number, or
    /// repeats a request ID.
    pub fn restore(host: &str, bytes: &[u8]) -> Result<(Self, usize), CorruptLine> {
        let mut log = Self::new(host);
        let mut valid = 0;
        for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
            if !line.ends_with(b"\n") {
                break;
            }
            let event: Event = serde_json::from_slice(line).map_err(|_| CorruptLine(index))?;
            if event.cursor != log.next_cursor() || log.by_request.contains_key(&event.request_id) {
                return Err(CorruptLine(index));
            }
            log.push(event);
            valid += line.len();
        }
        Ok((log, valid))
    }

    #[must_use]
    pub fn head(&self) -> Cursor {
        Cursor {
            host: self.host.clone(),
            seq: self.events.len() as u64,
        }
    }

    fn next_cursor(&self) -> Cursor {
        Cursor {
            host: self.host.clone(),
            seq: self.events.len() as u64 + 1,
        }
    }

    fn push(&mut self, event: Event) {
        self.by_request
            .insert(event.request_id.clone(), self.events.len());
        self.events.push(event);
    }

    /// Append a change and return the line the shell writes before it replies.
    ///
    /// # Panics
    /// Never: every field serializes as JSON.
    #[must_use]
    pub fn append(
        &mut self,
        request_id: &str,
        fingerprint: &str,
        operator: Operator,
        change: Change,
    ) -> (Event, Vec<u8>) {
        let event = Event {
            cursor: self.next_cursor(),
            request_id: request_id.to_owned(),
            fingerprint: fingerprint.to_owned(),
            operator,
            change,
        };
        let mut line = serde_json::to_vec(&event).expect("events always serialize");
        line.push(b'\n');
        self.push(event.clone());
        (event, line)
    }

    /// Every event after `after`, so a subscriber resumes without gaps or repeats.
    ///
    /// # Errors
    /// A cursor from another host, or one past the head.
    pub fn after(&self, after: Option<&Cursor>) -> Result<&[Event], ControlError> {
        let Some(cursor) = after else {
            return Ok(&self.events);
        };
        if cursor.host != self.host {
            return Err(ControlError::ForeignCursor);
        }
        usize::try_from(cursor.seq)
            .ok()
            .and_then(|seq| self.events.get(seq..))
            .ok_or(ControlError::CursorAhead)
    }

    /// Session detail from the live list and the spawn that created the session, if logged.
    #[must_use]
    pub fn session_detail(
        &self,
        workload: &StableId,
        session: &StableId,
        live: &[StableId],
    ) -> Option<SessionDetail> {
        let spawn = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.change {
                Change::SessionSpawned {
                    workload: w,
                    session: s,
                    socket,
                } if w == workload && s == session => Some((socket.clone(), event.operator)),
                _ => None,
            });
        let is_live = live.contains(session);
        if !is_live && spawn.is_none() {
            return None;
        }
        Some(SessionDetail {
            workload: workload.clone(),
            session: session.clone(),
            live: is_live,
            socket: spawn.as_ref().map(|(socket, _)| socket.clone()),
            spawned_by: spawn.map(|(_, operator)| operator),
        })
    }
}

/// What the daemon shell does with a forwarded request.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan<'a> {
    /// Answer a read from the driver's live state.
    Query,
    /// Run the mutation, then append its change under this request ID.
    Execute {
        request_id: String,
    },
    /// Send the matching events, then follow the log.
    Stream(&'a [Event]),
    Reply(Response),
}

/// Decide a request against the log. A repeated request ID with the same fingerprint replays
/// the logged outcome, and with a different fingerprint is refused.
#[must_use]
pub fn plan<'a>(log: &'a EventLog, forwarded: &Forwarded, fingerprint: &str) -> Plan<'a> {
    if let Request::Watch { after } = &forwarded.request {
        return match log.after(after.as_ref()) {
            Ok(events) => Plan::Stream(events),
            Err(error) => Plan::Reply(Response::Error(error)),
        };
    }
    if !forwarded.request.mutates() {
        return Plan::Query;
    }
    let Some(request_id) = forwarded.request_id.as_ref().filter(|id| !id.is_empty()) else {
        return Plan::Reply(Response::Error(ControlError::MissingRequestId));
    };
    match log
        .by_request
        .get(request_id)
        .map(|index| &log.events[*index])
    {
        None => Plan::Execute {
            request_id: request_id.clone(),
        },
        Some(event) if event.fingerprint == fingerprint => Plan::Reply(outcome(event)),
        Some(_) => Plan::Reply(Response::Error(ControlError::RequestIdReused)),
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
