//! Query replies, modes, and sizes on small hand-written inputs. The recorded sessions are
//! replayed against ghostty-vt in `crates/hypervisor-ghostty/tests/recordings.rs`.

use hypervisor_alacritty::AlacrittyEmulator;
use hypervisor_core::emulator::{Emulator, MouseFormat, MouseTracking, Screen, Size};

fn emulator() -> AlacrittyEmulator {
    AlacrittyEmulator::new(Size::new(80, 24).expect("size"))
}

fn reply(input: &[u8]) -> String {
    String::from_utf8(emulator().feed(input)).expect("UTF-8 reply")
}

#[test]
fn identity_queries_are_answered_from_the_core_profile() {
    assert_eq!(reply(b"\x1b[c"), "\x1b[?62;22c");
    assert_eq!(reply(b"\x1b[>c"), "\x1b[>1;10;0c");
    assert_eq!(reply(b"\x1b[=c"), "\x1bP!|00000000\x1b\\");
    assert_eq!(reply(b"\x1b[>q"), "\x1bP>|agent-hypervisor\x1b\\");
}

#[test]
fn replies_keep_the_order_of_their_queries() {
    assert_eq!(
        reply(b"\x1b[>q\x1b[3;5H\x1b[6n\x1b[c\x1b[?2026$p"),
        "\x1bP>|agent-hypervisor\x1b\\\x1b[3;5R\x1b[?62;22c\x1b[?2026;2$y"
    );
}

#[test]
fn a_query_split_across_feeds_is_answered_once() {
    let mut emu = emulator();
    assert!(emu.feed(b"\x1b[").is_empty());
    assert_eq!(emu.feed(b">q"), b"\x1bP>|agent-hypervisor\x1b\\");
}

#[test]
fn kitty_keyboard_query_goes_unanswered_but_pushed_flags_are_kept() {
    let mut emu = emulator();
    assert_eq!(emu.feed(b"\x1b[?u\x1b[c"), b"\x1b[?62;22c");
    emu.feed(b"\x1b[>5u");
    assert_eq!(emu.modes().kitty_keyboard_flags, 5);
    assert!(emu.feed(b"\x1b[?u").is_empty());
    emu.feed(b"\x1b[<u");
    assert_eq!(emu.modes().kitty_keyboard_flags, 0);
}

#[test]
fn modes_follow_the_sequences() {
    let mut emu = emulator();
    let start = emu.modes();
    assert_eq!(start.screen, Screen::Primary);
    assert!(start.cursor_visible && start.wraparound && start.alternate_scroll);
    emu.feed(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[?1h\x1b=");
    emu.feed(b"\x1b[?2026h\x1b[?2027h");
    let m = emu.modes();
    assert_eq!(m.screen, Screen::Alternate);
    assert_eq!(m.mouse_tracking, MouseTracking::Button);
    assert_eq!(m.mouse_format, MouseFormat::Sgr);
    assert!(m.bracketed_paste && m.focus_events && m.application_cursor_keys);
    assert!(m.application_keypad && m.synchronized_output && m.grapheme_clustering);
    assert!(!m.cursor_visible);
    emu.feed(b"\x1b[?1049l\x1b[?1002l\x1b[?2026l");
    let m = emu.modes();
    assert_eq!(m.screen, Screen::Primary);
    assert_eq!(m.mouse_tracking, MouseTracking::Off);
    assert!(!m.synchronized_output);
}

#[test]
fn modes_alacritty_ignores_stay_at_their_defaults() {
    let mut emu = emulator();
    emu.feed(b"\x1b[?9h\x1b[?1015h\x1b[?1016h");
    assert_eq!(emu.modes().mouse_tracking, MouseTracking::Off);
    assert_eq!(emu.modes().mouse_format, MouseFormat::X10);
}

#[test]
fn a_synchronized_update_is_drawn_before_it_ends() {
    let mut emu = emulator();
    emu.feed(b"\x1b[?2026hframe");
    assert_eq!(emu.grid().text_rows()[0], "frame");
}

#[test]
fn resize_changes_the_size() {
    let mut emu = emulator();
    let size = Size::new(132, 50).expect("size");
    emu.resize(size).expect("resize");
    assert_eq!(emu.size(), size);
    assert_eq!(emu.grid().size(), size);
}
