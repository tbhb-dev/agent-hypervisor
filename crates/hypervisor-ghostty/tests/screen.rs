//! Recorded screen checkpoints from RFC-36 runs 3 and 5, plus their final grids.

#[allow(
    dead_code,
    reason = "shared fixture helpers include fields used by other test targets"
)]
mod common;

use common::{Fixture, fixture};
use hypervisor_alacritty::AlacrittyEmulator;
use hypervisor_core::emulator::Emulator;
use hypervisor_core::screen::{RuleDetector, ScreenDetector};
use hypervisor_core::state::{AgentState, BlockReason, Harness};
use hypervisor_ghostty::GhosttyEmulator;
use serde_json::Value;

fn detect<E: Emulator>(emulator: &E, harness: Harness) -> Option<AgentState> {
    RuleDetector::default().detect(harness, emulator.title().as_deref(), &emulator.grid())
}

fn at_prefix(f: &Fixture, count: usize) -> (GhosttyEmulator, AlacrittyEmulator) {
    let mut ghostty = GhosttyEmulator::new(f.size).unwrap();
    let mut alacritty = AlacrittyEmulator::new(f.size);
    ghostty.feed(&f.bytes[..count]);
    alacritty.feed(&f.bytes[..count]);
    (ghostty, alacritty)
}

#[test]
fn both_emulators_expose_osc_8_cell_uri() {
    let size = hypervisor_core::emulator::Size::new(2, 1).unwrap();
    let mut ghostty = GhosttyEmulator::new(size).unwrap();
    let mut alacritty = AlacrittyEmulator::new(size);
    let input = b"\x1b]8;;https://example.test/a\x1b\\X\x1b]8;;\x1b\\Y";
    ghostty.feed(input);
    alacritty.feed(input);
    assert_eq!(
        ghostty.grid().cells()[0].hyperlink.as_deref(),
        Some("https://example.test/a")
    );
    assert_eq!(
        alacritty.grid().cells()[0].hyperlink.as_deref(),
        Some("https://example.test/a")
    );
    assert_eq!(ghostty.grid().cells()[1].hyperlink, None);
    assert_eq!(alacritty.grid().cells()[1].hyperlink, None);
}

#[test]
fn recorded_idle_approval_and_final_grids_follow_screen_rules() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/screen-checkpoints.json"
    );
    let cells: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let cells = cells.as_array().unwrap();
    assert_eq!(cells.len(), 15);
    for cell in cells {
        let name = cell["cell"].as_str().unwrap();
        let f = fixture(name);
        let harness = if name.starts_with("claude") {
            Harness::Claude
        } else if name.starts_with("codex") {
            Harness::Codex
        } else {
            Harness::Agy
        };
        for (stage, expected) in [
            ("idle_bytes", Some(AgentState::Idle)),
            (
                "approval_bytes",
                Some(AgentState::Blocked {
                    reason: BlockReason::Approval,
                }),
            ),
        ] {
            let count = usize::try_from(cell[stage].as_u64().unwrap()).unwrap();
            let (ghostty, alacritty) = at_prefix(&f, count);
            assert_eq!(
                detect(&ghostty, harness),
                expected,
                "{name} {stage} ghostty"
            );
            assert_eq!(
                detect(&alacritty, harness),
                expected,
                "{name} {stage} alacritty"
            );
        }
        let (ghostty, alacritty) = at_prefix(&f, f.bytes.len());
        let final_state =
            if name.starts_with("claude-default") || name == "claude-fullscreen-noalt-answered" {
                Some(AgentState::Blocked {
                    reason: BlockReason::Approval,
                })
            } else {
                None
            };
        assert_eq!(
            detect(&ghostty, harness),
            final_state,
            "{name} final ghostty"
        );
        assert_eq!(
            detect(&alacritty, harness),
            final_state,
            "{name} final alacritty"
        );
        println!("{name}: idle, approval, final grids matched in both emulators");
    }
}
