//! Conformance: replay the recorded harness sessions through the ghostty-vt emulator.
//!
//! The recordings and their expected final screens come from RFC-36 runs 2, 3, and 5; see
//! `tests/fixtures/README.md` for provenance. Each fixture is checked six ways: the final screen
//! text and cursor, the `alacritty_terminal` cross-check, the grid serializer round trip, the
//! binary snapshot round trip, query replies with no viewer, and resizing after the replay.

mod common;

use common::{FIXTURES, Fixture, alacritty_grid, comparable, fixture};
use hypervisor_core::emulator::{CellDiff, CellWidth, Emulator, Grid, Size, diff};
use hypervisor_ghostty::GhosttyEmulator;

fn replay(f: &Fixture) -> (GhosttyEmulator, Vec<u8>) {
    let mut emu = GhosttyEmulator::new(f.size).expect("terminal");
    let replies = emu.feed(&f.bytes);
    (emu, replies)
}

fn fresh_grid(size: Size, bytes: &[u8]) -> Grid {
    let mut emu = GhosttyEmulator::new(size).expect("terminal");
    emu.feed(bytes);
    emu.grid()
}

fn describe(diffs: &[CellDiff]) -> String {
    diffs
        .iter()
        .take(5)
        .map(|d| format!("({},{}) {:?} vs {:?}", d.row, d.col, d.left, d.right))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn final_screen_matches_the_spike_record() {
    for name in FIXTURES {
        let f = fixture(name);
        let (emu, _) = replay(&f);
        let grid = emu.grid();
        assert_eq!(grid.text_rows(), f.text, "{name}: screen text");
        assert_eq!(grid.cursor(), f.cursor, "{name}: cursor");
    }
}

#[test]
fn feeding_in_small_chunks_gives_the_same_grid() {
    for name in FIXTURES {
        let f = fixture(name);
        let (whole, _) = replay(&f);
        let mut chunked = GhosttyEmulator::new(f.size).expect("terminal");
        for chunk in f.bytes.chunks(7) {
            chunked.feed(chunk);
        }
        let d = diff(&whole.grid(), &chunked.grid());
        assert!(
            d.is_empty(),
            "{name}: {} cells differ\n{}",
            d.len(),
            describe(&d)
        );
    }
}

#[test]
fn alacritty_agrees_except_for_regional_indicator_width() {
    for name in FIXTURES {
        let f = fixture(name);
        let (emu, _) = replay(&f);
        let ghostty = comparable(&emu.grid());
        let alacritty = alacritty_grid(f.size, &f.bytes);
        let d = diff(&ghostty, &alacritty);
        assert_eq!(
            d.len(),
            f.alacritty_grid_diffs,
            "{name}: grid differences\n{}",
            describe(&d)
        );
        assert_eq!(ghostty.cursor(), alacritty.cursor(), "{name}: cursor");
    }
    // Spike 2's 30 differences all sit on the emoji row and start at the flag: ghostty-vt makes
    // each regional indicator wide, alacritty_terminal narrow, and the rest of the row shifts.
    let f = fixture("synthetic-alt");
    let (emu, _) = replay(&f);
    let d = diff(&comparable(&emu.grid()), &alacritty_grid(f.size, &f.bytes));
    assert!(d.iter().all(|d| d.row == 5), "only row 5 differs");
    let first = &d[0];
    assert_eq!((first.col, first.left.text.as_str()), (29, "\u{1f1ef}"));
    assert_eq!(first.left.width, CellWidth::Wide);
    assert_eq!(first.right.width, CellWidth::Narrow);
}

#[test]
fn serialized_screen_redraws_the_same_grid() {
    for name in FIXTURES {
        let f = fixture(name);
        let (emu, _) = replay(&f);
        let original = emu.grid();
        let redrawn = fresh_grid(f.size, &emu.serialize_vt());
        let d = diff(&original, &redrawn);
        assert!(
            d.is_empty(),
            "{name}: {} cells differ\n{}",
            d.len(),
            describe(&d)
        );
        assert_eq!(original.cursor(), redrawn.cursor(), "{name}: cursor");

        let mut again = GhosttyEmulator::new(f.size).expect("terminal");
        again.feed(&emu.serialize_vt());
        let mut expected_modes = emu.modes();
        expected_modes.synchronized_output = false;
        expected_modes.kitty_keyboard_flags = 0;
        assert_eq!(again.modes(), expected_modes, "{name}: modes");
    }
}

#[test]
fn serialized_screen_redraws_in_alacritty_too() {
    for name in FIXTURES {
        let f = fixture(name);
        let (emu, _) = replay(&f);
        let original = comparable(&emu.grid());
        let redrawn = alacritty_grid(f.size, &emu.serialize_vt());
        let d = diff(&original, &redrawn);
        assert_eq!(
            d.len(),
            f.alacritty_grid_diffs,
            "{name}: grid differences\n{}",
            describe(&d)
        );
    }
}

#[test]
fn binary_snapshot_restores_grid_and_modes() {
    for name in FIXTURES {
        let f = fixture(name);
        let (emu, _) = replay(&f);
        let snapshot = emu.snapshot().expect("snapshot");
        let restored = GhosttyEmulator::restore(&snapshot).expect("restore");
        let d = diff(&emu.grid(), &restored.grid());
        assert!(
            d.is_empty(),
            "{name}: {} cells differ\n{}",
            d.len(),
            describe(&d)
        );
        assert_eq!(
            emu.grid().cursor(),
            restored.grid().cursor(),
            "{name}: cursor"
        );
        assert_eq!(emu.modes(), restored.modes(), "{name}: modes");
    }
}

#[test]
fn queries_are_answered_with_no_viewer_and_kitty_keyboard_is_not_claimed() {
    for name in FIXTURES.iter().filter(|n| !n.starts_with("synthetic")) {
        let f = fixture(name);
        let (_, replies) = replay(&f);
        let replies = String::from_utf8_lossy(&replies);
        assert!(contains(&f.bytes, b"\x1b[c"), "{name}: sends DA1");
        assert!(
            replies.contains("\x1b[?62;22c"),
            "{name}: DA1 answered: {replies:?}"
        );
        assert!(
            contains(&f.bytes, b"\x1b[?u"),
            "{name}: queries kitty keyboard"
        );
        assert!(!has_kitty_keyboard_reply(&replies), "{name}: {replies:?}");
    }
}

#[test]
fn resizing_after_a_replay_keeps_the_grid_readable() {
    let sizes = [(80, 24), (200, 60), (1, 1), (120, 40)];
    for name in FIXTURES {
        let f = fixture(name);
        let (mut emu, _) = replay(&f);
        for (cols, rows) in sizes {
            let size = Size::new(cols, rows).expect("size");
            emu.resize(size).expect("resize");
            let grid = emu.grid();
            assert_eq!(grid.size(), size, "{name}: size after resize");
            assert_eq!(emu.size(), size);
        }
        emu.feed(b"\x1b[Hafter");
        assert!(emu.grid().text_rows()[0].starts_with("after"), "{name}");
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn has_kitty_keyboard_reply(replies: &str) -> bool {
    replies.split('\x1b').any(|r| {
        r.strip_prefix("[?")
            .and_then(|r| r.strip_suffix('u'))
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    })
}
