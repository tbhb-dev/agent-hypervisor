use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hypervisor_core::state::{
    AgentState, BlockReason, Harness, HookKind, HookReport, StateMachine,
};
use hypervisord::{HookSocket, trimmed_event};

fn expected(name: &str) -> AgentState {
    match name {
        "Unknown" => AgentState::Unknown,
        "Idle" => AgentState::Idle,
        "Working" => AgentState::Working,
        "Approval" => AgentState::Blocked {
            reason: BlockReason::Approval,
        },
        "Exited" => AgentState::Exited,
        _ => panic!("unknown state {name}"),
    }
}

#[test]
fn recorded_hook_cells_replay_each_state() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook-replay");
    let mut count = 0;
    for file in fs::read_dir(root).unwrap() {
        let file = file.unwrap();
        let name = file.file_name().into_string().unwrap();
        let harness = if name.starts_with("claude") {
            Harness::Claude
        } else if name.starts_with("codex") {
            Harness::Codex
        } else {
            Harness::Agy
        };
        let mut machine = StateMachine::default();
        for (index, line) in fs::read_to_string(file.path()).unwrap().lines().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let event = trimmed_event(harness, &value);
            let kind = HookKind::from_fields(
                harness,
                event["event"].as_str().unwrap(),
                event["notification_type"].as_str(),
                event["fully_idle"].as_bool().unwrap_or(false),
            );
            machine.report(HookReport {
                harness,
                kind,
                seq: u64::try_from(index + 1).unwrap(),
                at: Duration::from_millis(value["at_ms"].as_u64().unwrap()),
            });
            assert_eq!(
                machine.state(),
                expected(value["expected"].as_str().unwrap()),
                "{name} event {}",
                index + 1
            );
        }
        if let Some(deadline) = machine.deadline() {
            assert_eq!(
                machine.tick(deadline),
                Some(AgentState::Blocked {
                    reason: BlockReason::Unknown
                }),
                "{name} unanswered tool timeout"
            );
        }
        println!("{name}: {:?}", machine.state());
        count += 1;
    }
    assert_eq!(count, 15);
}

#[test]
fn hook_client_delivers_and_fails_open() {
    let root = std::env::temp_dir().join(format!("hv-hook-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let mut socket = HookSocket::bind(&root, "workspace", "session").unwrap();
    assert!(socket.path().as_os_str().len() <= 103);
    let input =
        br#"{"hook_event_name":"PermissionRequest","session_id":"claimed","prompt":"private"}"#;
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hypervisord"))
        .args(["hook", "claude", socket.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let received = socket.receive_one().unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(received.kind, HookKind::PermissionRequest);
    assert_eq!(received.claimed_session_id.as_deref(), Some("claimed"));
    assert_eq!(received.seq, 1);
    println!("live listener: {:?}", start.elapsed());
    let missing = root.join("missing.s");
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hypervisord"))
        .args(["hook", "agy", missing.to_str().unwrap(), "PreToolUse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\"decision\":\"ask\"}\n");
    assert!(start.elapsed() < Duration::from_millis(500));
    println!("missing listener: {:?}", start.elapsed());

    let stalled = HookSocket::bind(&root, "workspace", "stalled").unwrap();
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hypervisord"))
        .args(["hook", "claude", stalled.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(start.elapsed() < Duration::from_secs(1));
    println!("stalled listener: {:?}", start.elapsed());
    drop(stalled);
    drop(socket);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn socket_directory_is_private_and_identity_is_listener() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("hv-hook-mode-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let mut socket = HookSocket::bind(&root, "workspace", "session").unwrap();
    assert_eq!(
        fs::metadata(socket.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let mut stream = UnixStream::connect(socket.path()).unwrap();
    stream
        .write_all(b"{\"harness\":\"codex\",\"event\":\"Stop\",\"claimed_session_id\":\"some-other-session\"}\n")
        .unwrap();
    let report = socket.receive_one().unwrap();
    drop(stream);
    assert_eq!(
        report.claimed_session_id.as_deref(),
        Some("some-other-session")
    );
    assert_eq!(report.kind, HookKind::Stop { fully_idle: false });
    drop(socket);
    let _ = fs::remove_dir_all(root);
}
