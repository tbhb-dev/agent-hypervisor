//! Golden tests: replay every recording in `tests/fixtures/corpus/` through ghostty-vt and
//! `alacritty_terminal` and compare the grids.
//!
//! The corpus `README.md` gives each recording's provenance, and its JSON file the final screen
//! and the accepted number of cells where the two emulators differ. A new difference fails, and an
//! accepted one is recorded in the JSON with its cause. Each recording is also checked for its
//! final screen, chunked feeding, the grid serializer round trip in both emulators, the binary
//! snapshot round trip, query replies with no viewer, and resizing after the replay.

mod common;

use common::{Fixture, comparable, fixture, fixtures};
use hypervisor_alacritty::AlacrittyEmulator;
use hypervisor_core::emulator::{
    CellDiff, CellWidth, Emulator, Modes, MouseFormat, MouseTracking, Screen, Size, diff,
};
use hypervisor_ghostty::GhosttyEmulator;
use sha2::{Digest, Sha256};

fn ghostty(f: &Fixture) -> (GhosttyEmulator, Vec<u8>) {
    let mut emu = GhosttyEmulator::new(f.size).expect("terminal");
    let replies = emu.feed(&f.bytes);
    (emu, replies)
}

fn alacritty(f: &Fixture) -> (AlacrittyEmulator, Vec<u8>) {
    let mut emu = AlacrittyEmulator::new(f.size);
    let replies = emu.feed(&f.bytes);
    (emu, replies)
}

fn redraw<E: Emulator>(mut fresh: E, from: &impl Emulator) -> E {
    fresh.feed(&from.serialize_vt());
    fresh
}

fn describe(diffs: &[CellDiff]) -> String {
    diffs
        .iter()
        .take(5)
        .map(|d| format!("({},{}) {:?} vs {:?}", d.row, d.col, d.left, d.right))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ghostty's modes as alacritty can report them: alacritty ignores modes 9, 1015, and 1016.
fn comparable_modes(m: Modes) -> Modes {
    Modes {
        mouse_tracking: match m.mouse_tracking {
            MouseTracking::X10 => MouseTracking::Off,
            other => other,
        },
        mouse_format: match m.mouse_format {
            MouseFormat::Urxvt | MouseFormat::SgrPixels => MouseFormat::X10,
            other => other,
        },
        ..m
    }
}

/// The modes `serialize_vt` restores: it never sets 2026 or pushes kitty keyboard flags.
fn restorable(m: Modes) -> Modes {
    Modes {
        synchronized_output: false,
        kitty_keyboard_flags: 0,
        ..m
    }
}

#[test]
fn recording_hashes_match_the_metadata() {
    for f in fixtures() {
        assert_eq!(
            format!("{:x}", Sha256::digest(&f.bytes)),
            f.sha256,
            "{}: SHA-256",
            f.name
        );
    }
}

#[test]
fn final_screen_matches_the_record() {
    for f in fixtures() {
        let (emu, _) = ghostty(&f);
        let grid = emu.grid();
        assert_eq!(grid.text_rows(), f.text, "{}: screen text", f.name);
        assert_eq!(grid.cursor(), f.cursor, "{}: cursor", f.name);
        assert_eq!(emu.modes().screen, f.screen, "{}: active screen", f.name);
    }
}

#[test]
fn feeding_in_small_chunks_gives_the_same_grid() {
    for f in fixtures() {
        let mut g = GhosttyEmulator::new(f.size).expect("terminal");
        let mut a = AlacrittyEmulator::new(f.size);
        for chunk in f.bytes.chunks(7) {
            g.feed(chunk);
            a.feed(chunk);
        }
        let d = diff(&ghostty(&f).0.grid(), &g.grid());
        assert!(d.is_empty(), "{}: ghostty\n{}", f.name, describe(&d));
        let d = diff(&alacritty(&f).0.grid(), &a.grid());
        assert!(d.is_empty(), "{}: alacritty\n{}", f.name, describe(&d));
    }
}

#[test]
fn alacritty_matches_ghostty_on_every_recording() {
    let mut failures = Vec::new();
    for f in fixtures() {
        let (g, _) = ghostty(&f);
        let (a, _) = alacritty(&f);
        let (gg, ag) = (comparable(&g.grid()), a.grid());
        let d = diff(&gg, &ag);
        println!("{}: {} cells differ", f.name, d.len());
        if d.len() != f.alacritty_grid_diffs {
            failures.push(format!(
                "{}: {} cells differ, {} accepted\n{}",
                f.name,
                d.len(),
                f.alacritty_grid_diffs,
                describe(&d)
            ));
        }
        if gg.cursor() != ag.cursor() {
            failures.push(format!(
                "{}: cursor {:?} vs {:?}",
                f.name,
                gg.cursor(),
                ag.cursor()
            ));
        }
        if comparable_modes(g.modes()) != a.modes() {
            failures.push(format!(
                "{}: modes {:?} vs {:?}",
                f.name,
                g.modes(),
                a.modes()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn regional_indicator_width_is_the_known_difference() {
    // Spike 2's 30 differences all sit on the emoji row and start at the flag: ghostty-vt makes
    // each regional indicator wide, alacritty_terminal narrow, and the rest of the row shifts.
    let f = fixture("synthetic-alt");
    let d = diff(&comparable(&ghostty(&f).0.grid()), &alacritty(&f).0.grid());
    assert_eq!(d.len(), 30);
    assert!(d.iter().all(|d| d.row == 5), "only row 5 differs");
    let first = &d[0];
    assert_eq!((first.col, first.left.text.as_str()), (29, "\u{1f1ef}"));
    assert_eq!(first.left.width, CellWidth::Wide);
    assert_eq!(first.right.width, CellWidth::Narrow);
}

#[test]
fn serialized_screen_redraws_the_same_grid_in_each_emulator() {
    for f in fixtures() {
        let (g, _) = ghostty(&f);
        let again = redraw(GhosttyEmulator::new(f.size).expect("terminal"), &g);
        let d = diff(&g.grid(), &again.grid());
        println!("{}: ghostty round trip {} cells differ", f.name, d.len());
        assert!(d.is_empty(), "{}: ghostty\n{}", f.name, describe(&d));
        assert_eq!(g.grid().cursor(), again.grid().cursor(), "{}", f.name);
        assert_eq!(again.modes(), restorable(g.modes()), "{}: modes", f.name);

        let (a, _) = alacritty(&f);
        let again = redraw(AlacrittyEmulator::new(f.size), &a);
        let d = diff(&a.grid(), &again.grid());
        println!("{}: alacritty round trip {} cells differ", f.name, d.len());
        assert!(d.is_empty(), "{}: alacritty\n{}", f.name, describe(&d));
        assert_eq!(a.grid().cursor(), again.grid().cursor(), "{}", f.name);
        assert_eq!(again.modes(), restorable(a.modes()), "{}: modes", f.name);
    }
}

#[test]
fn ghostty_serialized_screen_redraws_in_alacritty() {
    for f in fixtures() {
        let (g, _) = ghostty(&f);
        let a = redraw(AlacrittyEmulator::new(f.size), &g);
        let d = diff(&comparable(&g.grid()), &a.grid());
        assert_eq!(
            d.len(),
            f.alacritty_grid_diffs,
            "{}: grid differences\n{}",
            f.name,
            describe(&d)
        );
    }
}

#[test]
fn binary_snapshot_restores_grid_and_modes() {
    for f in fixtures() {
        let (mut emu, _) = ghostty(&f);
        let snapshot = emu.snapshot().expect("snapshot");
        let mut restored = GhosttyEmulator::restore(&snapshot).expect("restore");
        let d = diff(&emu.grid(), &restored.grid());
        assert!(d.is_empty(), "{}\n{}", f.name, describe(&d));
        assert_eq!(emu.grid().cursor(), restored.grid().cursor(), "{}", f.name);
        assert_eq!(emu.modes(), restored.modes(), "{}: modes", f.name);
        for query in [&b"\x1b[c"[..], b"\x1b[6n", b"\x1b[?2026$p", b"\x1b[?u"] {
            let reply = restored.feed(query);
            assert_eq!(
                reply,
                emu.feed(query),
                "{}: restored reply to {query:?}",
                f.name
            );
            if query == b"\x1b[c" {
                assert_eq!(reply, b"\x1b[?62;22c", "{}: restored DA1", f.name);
            }
            if query == b"\x1b[?u" {
                assert!(reply.is_empty(), "{}: restored kitty query", f.name);
            }
        }
    }
}

#[test]
fn both_emulators_answer_queries_alike_with_no_viewer() {
    for f in fixtures() {
        let (_, g) = ghostty(&f);
        let (_, a) = alacritty(&f);
        let replies = String::from_utf8_lossy(&g);
        if contains(&f.bytes, b"\x1b[c") {
            assert!(
                replies.contains("\x1b[?62;22c"),
                "{}: DA1 {replies:?}",
                f.name
            );
        }
        assert!(
            !has_kitty_keyboard_reply(&replies),
            "{}: {replies:?}",
            f.name
        );
        let expected = KNOWN_REPLY_DIFFERENCES
            .iter()
            .fold(replies.to_string(), |r, (g, a)| r.replace(g, a));
        assert_eq!(String::from_utf8_lossy(&a), expected, "{}", f.name);
    }
}

/// Replies where the emulators disagree, as ghostty-vt's reply and alacritty's. In DECRQM,
/// alacritty does not recognize mode 1016, SGR pixel mouse reports, or mode 2027, because it does
/// no grapheme clustering.
const KNOWN_REPLY_DIFFERENCES: [(&str, &str); 2] = [
    ("\x1b[?1016;2$y", "\x1b[?1016;0$y"),
    ("\x1b[?2027;2$y", "\x1b[?2027;0$y"),
];

#[test]
fn resizing_after_a_replay_keeps_the_grid_readable() {
    let sizes = [(80, 24), (200, 60), (1, 1), (120, 40)];
    for f in fixtures() {
        let (mut g, _) = ghostty(&f);
        let (mut a, _) = alacritty(&f);
        for (cols, rows) in sizes {
            let size = Size::new(cols, rows).expect("size");
            g.resize(size).expect("resize");
            a.resize(size).expect("resize");
            assert_eq!(
                (g.grid().size(), a.grid().size()),
                (size, size),
                "{}",
                f.name
            );
        }
        g.feed(b"\x1b[Hafter");
        a.feed(b"\x1b[Hafter");
        assert!(g.grid().text_rows()[0].starts_with("after"), "{}", f.name);
        assert!(a.grid().text_rows()[0].starts_with("after"), "{}", f.name);
    }
}

#[test]
fn shrinking_and_restoring_a_fullscreen_session_matches_across_emulators() {
    // zmx documents restore edge cases at mismatched sizes (RFC-36 run 4). Replay each recording
    // that used the alternate screen up to its last exit from it, shrink both emulators, grow
    // them back, and compare what each keeps.
    let small = Size::new(80, 24).expect("size");
    let mut seen = Vec::new();
    for f in fixtures() {
        let Some(end) = rfind(&f.bytes, b"\x1b[?1049l")
            .or_else(|| contains(&f.bytes, b"\x1b[?1049h").then_some(f.bytes.len()))
        else {
            continue;
        };
        let mut g = GhosttyEmulator::new(f.size).expect("terminal");
        let mut a = AlacrittyEmulator::new(f.size);
        g.feed(&f.bytes[..end]);
        a.feed(&f.bytes[..end]);
        assert_eq!(g.modes().screen, Screen::Alternate, "{}", f.name);
        let mut counts = Vec::new();
        for size in [small, f.size] {
            g.resize(size).expect("resize");
            a.resize(size).expect("resize");
            let d = diff(&comparable(&g.grid()), &a.grid());
            println!("{}: at {size:?} {} cells differ", f.name, d.len());
            counts.push(d.len());
        }
        seen.push((f.name.clone(), counts));
    }
    let expected: Vec<(String, Vec<usize>)> = RESIZE_DIFFS
        .iter()
        .map(|(n, c)| ((*n).to_owned(), c.to_vec()))
        .collect();
    assert_eq!(seen, expected);
}

/// Cells that differ between the emulators after the shrink to 80 by 24 and after growing back,
/// per recording that used the alternate screen. Every nonzero count has one cause: on a shrink,
/// ghostty-vt keeps the bottom rows of the alternate screen, and `alacritty_terminal` keeps the top
/// rows while the cursor still fits. Growing back restores neither.
const RESIZE_DIFFS: [(&str, [usize; 2]); 12] = [
    ("agy-alt-answered", [0, 0]),
    ("agy-alt-ignored", [0, 0]),
    ("agy-default-answered", [0, 0]),
    ("agy-reviewdefault-nopretool-answered", [0, 0]),
    ("claude-fullscreen-answered", [1166, 1166]),
    ("claude-fullscreen-ignored", [1164, 1164]),
    ("codex-auto-answered", [0, 0]),
    ("codex-auto-ignored", [0, 0]),
    ("shell-htop-answered", [0, 0]),
    ("shell-vim-answered", [0, 0]),
    ("synthetic-alt", [693, 693]),
    ("synthetic-exit", [693, 693]),
];

#[test]
fn alternate_screen_shrink_keeps_opposite_ends() {
    let f = fixture("synthetic-alt");
    let rows = ghostty(&f).0.grid().text_rows();
    let small = Size::new(120, 24).expect("size");
    let (mut g, _) = ghostty(&f);
    let (mut a, _) = alacritty(&f);
    g.resize(small).expect("resize");
    a.resize(small).expect("resize");
    assert_eq!(
        g.grid().text_rows(),
        rows[16..],
        "ghostty-vt keeps the bottom 24 rows"
    );
    assert_eq!(
        a.grid().text_rows(),
        rows[..24],
        "alacritty keeps the top 24 rows"
    );
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
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
