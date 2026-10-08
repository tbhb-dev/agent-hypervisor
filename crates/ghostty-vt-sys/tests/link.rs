//! The static archive links, and the library runs.
#![allow(unsafe_code, reason = "these tests call the raw FFI")]

use ghostty_vt_sys as sys;

#[test]
fn a_terminal_can_be_created_written_and_freed() {
    let mut term: sys::GhosttyTerminal = std::ptr::null_mut();
    let mut col: u16 = 0;
    // SAFETY: a null allocator selects the default; every pointer is valid for its call, and the
    // terminal is freed once.
    unsafe {
        let code = sys::ghostty_terminal_new(std::ptr::null(), &raw mut term, 80, 24);
        assert_eq!(code, sys::GHOSTTY_SUCCESS);
        let text = b"hello";
        sys::ghostty_terminal_vt_write(term, text.as_ptr(), text.len());
        let code = sys::ghostty_terminal_get(
            term,
            sys::GHOSTTY_TERMINAL_DATA_CURSOR_X,
            (&raw mut col).cast(),
        );
        assert_eq!(code, sys::GHOSTTY_SUCCESS);
        sys::ghostty_terminal_free(term);
    }
    assert_eq!(col, 5);
}

#[test]
fn the_pin_is_a_full_commit_hash() {
    assert_eq!(sys::GHOSTTY_COMMIT.len(), 40);
    assert!(sys::GHOSTTY_COMMIT.bytes().all(|b| b.is_ascii_hexdigit()));
}
