//! Query replies, modes, and sizes on small hand-written inputs.

use hypervisor_core::emulator::{
    Emulator, MouseFormat, MouseTracking, Screen, Size, XTVERSION_NAME, diff,
};
use hypervisor_ghostty::GhosttyEmulator;

fn emulator() -> GhosttyEmulator {
    GhosttyEmulator::new(Size::new(80, 24).expect("size")).expect("terminal")
}

fn reply(input: &[u8]) -> String {
    String::from_utf8(emulator().feed(input)).expect("UTF-8 reply")
}

#[test]
fn device_attributes_report_a_vt220_with_ansi_color() {
    assert_eq!(reply(b"\x1b[c"), "\x1b[?62;22c");
    assert_eq!(reply(b"\x1b[>c"), "\x1b[>1;10;0c");
}

#[test]
fn xtversion_names_the_hypervisor() {
    let r = reply(b"\x1b[>q");
    assert!(r.contains(XTVERSION_NAME), "{r:?}");
}

#[test]
fn cursor_position_and_mode_reports_are_answered() {
    assert_eq!(reply(b"\x1b[3;5H\x1b[6n"), "\x1b[3;5R");
    assert_eq!(reply(b"\x1b[?2026$p"), "\x1b[?2026;2$y");
}

#[test]
fn kitty_keyboard_and_graphics_queries_go_unanswered() {
    assert_eq!(reply(b"\x1b[?u"), "");
    assert_eq!(reply(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"), "");
    // Only the DA1 sentinel after them is answered, as Claude Code and Codex send it.
    assert_eq!(reply(b"\x1b[?u\x1b[c"), "\x1b[?62;22c");
}

#[test]
fn replies_survive_a_snapshot_restore() {
    let emu = emulator();
    let mut restored =
        GhosttyEmulator::restore(&emu.snapshot().expect("snapshot")).expect("restore");
    assert_eq!(restored.feed(b"\x1b[c"), b"\x1b[?62;22c");
    assert_eq!(restored.feed(b"\x1b[?u"), b"");
}

#[test]
fn modes_follow_the_sequences() {
    let mut emu = emulator();
    let start = emu.modes();
    assert_eq!(start.screen, Screen::Primary);
    assert!(start.cursor_visible && start.wraparound);
    assert_eq!(start.kitty_keyboard_flags, 0);
    emu.feed(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[?1h\x1b[?2026h\x1b[>7u");
    let m = emu.modes();
    assert_eq!(m.screen, Screen::Alternate);
    assert_eq!(m.mouse_tracking, MouseTracking::Button);
    assert_eq!(m.mouse_format, MouseFormat::Sgr);
    assert!(m.bracketed_paste && m.focus_events && m.application_cursor_keys);
    assert!(m.synchronized_output);
    assert!(!m.cursor_visible);
    assert_eq!(m.kitty_keyboard_flags, 7);
    emu.feed(b"\x1b[?1049l\x1b[?1002l\x1b[?2026l");
    let m = emu.modes();
    assert_eq!(m.screen, Screen::Primary);
    assert_eq!(m.mouse_tracking, MouseTracking::Off);
    assert!(!m.synchronized_output);
}

#[test]
fn resize_changes_the_size() {
    let mut emu = emulator();
    let size = Size::new(132, 50).expect("size");
    emu.resize(size).expect("resize");
    assert_eq!(emu.size(), size);
    assert_eq!(emu.grid().size(), size);
}

#[test]
fn a_grapheme_longer_than_sixteen_codepoints_reads_back_whole() {
    let long = format!("e{}", "\u{301}".repeat(20));
    let mut emu = GhosttyEmulator::new(Size::new(10, 2).expect("size")).expect("terminal");
    emu.feed(long.as_bytes());
    assert_eq!(emu.grid().row(0)[0].text, long);
    let vt = emu.serialize_vt();
    assert!(!vt.contains(&0), "serialize_vt sent a NUL byte");
    let mut redrawn = GhosttyEmulator::new(emu.size()).expect("terminal");
    redrawn.feed(&vt);
    assert!(diff(&emu.grid(), &redrawn.grid()).is_empty());
}
