//! The [`Emulator`] on ghostty-vt, through `ghostty-vt-sys`.
//!
//! [`GhosttyEmulator`] owns one `GhosttyTerminal`. It is neither `Send` nor `Sync`: the library
//! requires every call on a terminal to be serialized, and RFC-36 keeps one thread per session,
//! so the session holder creates the emulator on that thread and never moves it. Herdr instead
//! marks its handles `Send` behind a `Mutex` (RFC-40 run 18); this crate does not.
//!
//! ```compile_fail
//! fn needs_send<T: Send>() {}
//! needs_send::<hypervisor_ghostty::GhosttyEmulator>();
//! ```
#![allow(
    unsafe_code,
    reason = "this crate is the safe wrapper over the libghostty-vt FFI"
)]

use std::ffi::c_void;
use std::fmt;
use std::mem::{size_of, zeroed};
use std::ptr::{self, NonNull};

use ghostty_vt_sys as sys;
use hypervisor_core::emulator::{
    self, Attrs, Cell, CellWidth, Color, Cursor, Emulator, Grid, Modes, MouseFormat, MouseTracking,
    PROFILE, Screen, Size, Underline,
};

/// A libghostty-vt call returned an error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyError {
    /// The function that failed.
    pub call: &'static str,
    /// Its `GhosttyResult`.
    pub code: i32,
}

impl fmt::Display for GhosttyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} returned {}", self.call, self.code)
    }
}

impl std::error::Error for GhosttyError {}

fn check(call: &'static str, code: sys::GhosttyResult) -> Result<(), GhosttyError> {
    if code == sys::GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(GhosttyError { call, code })
    }
}

/// Replies the terminal wrote during one `feed`, collected by the `write_pty` callback.
#[derive(Default)]
struct Replies(Vec<u8>);

/// A terminal emulator backed by ghostty-vt at the commit in `ghostty_vt_sys::GHOSTTY_COMMIT`.
pub struct GhosttyEmulator {
    term: NonNull<sys::GhosttyTerminalImpl>,
    /// Owned through `Box::into_raw` and freed in `Drop`; the callbacks get it as userdata.
    replies: NonNull<Replies>,
}

impl GhosttyEmulator {
    /// A blank terminal of `size` that answers queries on its own.
    ///
    /// # Errors
    ///
    /// When the library cannot allocate the terminal.
    pub fn new(size: Size) -> Result<Self, GhosttyError> {
        let mut term: sys::GhosttyTerminal = ptr::null_mut();
        // SAFETY: a null allocator selects the default; `term` is a valid out pointer.
        let code = unsafe {
            sys::ghostty_terminal_new(ptr::null(), &raw mut term, size.cols(), size.rows())
        };
        check("ghostty_terminal_new", code)?;
        Self::adopt(term)
    }

    /// The binary snapshot of the whole terminal: both screens, scrollback, and unfinished parser
    /// input. Use it for server-side copies and restarts. Its format has no compatibility promise
    /// across Ghostty commits.
    ///
    /// # Errors
    ///
    /// When the library cannot allocate the encoding.
    pub fn snapshot(&self) -> Result<Vec<u8>, GhosttyError> {
        let mut data: *mut u8 = ptr::null_mut();
        let mut len = 0usize;
        // SAFETY: the terminal is live; the out pointers are valid.
        let code = unsafe {
            sys::ghostty_snapshot_encode_alloc(
                self.term.as_ptr(),
                ptr::null(),
                &raw mut data,
                &raw mut len,
            )
        };
        check("ghostty_snapshot_encode_alloc", code)?;
        if data.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: the library returned `len` initialized bytes at `data`.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        // SAFETY: `data` came from the default allocator with this length.
        unsafe { sys::ghostty_free(ptr::null(), data, len) };
        Ok(bytes)
    }

    /// A terminal restored from [`GhosttyEmulator::snapshot`] bytes.
    ///
    /// # Errors
    ///
    /// When the bytes do not decode.
    pub fn restore(snapshot: &[u8]) -> Result<Self, GhosttyError> {
        let mut decoder: sys::GhosttySnapshotDecoder = ptr::null_mut();
        // SAFETY: the buffer outlives the decoder, which is freed below.
        let code = unsafe {
            sys::ghostty_snapshot_decoder_new_buf(
                ptr::null(),
                &raw mut decoder,
                snapshot.as_ptr(),
                snapshot.len(),
            )
        };
        check("ghostty_snapshot_decoder_new_buf", code)?;
        let mut term: sys::GhosttyTerminal = ptr::null_mut();
        // SAFETY: `decoder` is live; `term` is a valid out pointer.
        let code = unsafe { sys::ghostty_snapshot_decoder_decode(decoder, &raw mut term) };
        // SAFETY: the decoder is not used after this.
        unsafe { sys::ghostty_snapshot_decoder_free(decoder) };
        check("ghostty_snapshot_decoder_decode", code)?;
        Self::adopt(term)
    }

    fn adopt(term: sys::GhosttyTerminal) -> Result<Self, GhosttyError> {
        let term = NonNull::new(term).ok_or(GhosttyError {
            call: "terminal handle",
            code: sys::GHOSTTY_INVALID_VALUE,
        })?;
        let replies = NonNull::from(Box::leak(Box::new(Replies::default())));
        let emulator = Self { term, replies };
        emulator.install_effects()?;
        Ok(emulator)
    }

    fn install_effects(&self) -> Result<(), GhosttyError> {
        let t = self.term.as_ptr();
        // Zero storage turns the kitty graphics protocol off, so its query gets no reply.
        let no_images: u64 = 0;
        // SAFETY: each option takes the documented value type. Callback options take the function
        // pointer itself; the userdata stays valid until `Drop` frees it after the terminal.
        unsafe {
            check(
                "set userdata",
                sys::ghostty_terminal_set(
                    t,
                    sys::GHOSTTY_TERMINAL_OPT_USERDATA,
                    self.replies.as_ptr().cast::<c_void>(),
                ),
            )?;
            check(
                "set write_pty",
                sys::ghostty_terminal_set(
                    t,
                    sys::GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                    on_write_pty as *const c_void,
                ),
            )?;
            check(
                "set device_attributes",
                sys::ghostty_terminal_set(
                    t,
                    sys::GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES,
                    on_device_attributes as *const c_void,
                ),
            )?;
            check(
                "set xtversion",
                sys::ghostty_terminal_set(
                    t,
                    sys::GHOSTTY_TERMINAL_OPT_XTVERSION,
                    on_xtversion as *const c_void,
                ),
            )?;
            check(
                "set kitty image storage",
                sys::ghostty_terminal_set(
                    t,
                    sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                    (&raw const no_images).cast::<c_void>(),
                ),
            )?;
        }
        Ok(())
    }

    fn get<T: Copy>(&self, data: sys::GhosttyTerminalData, mut value: T) -> Option<T> {
        // SAFETY: the caller passes the output type the header documents for `data`.
        let code = unsafe {
            sys::ghostty_terminal_get(self.term.as_ptr(), data, (&raw mut value).cast::<c_void>())
        };
        (code == sys::GHOSTTY_SUCCESS).then_some(value)
    }

    fn mode(&self, value: u16) -> bool {
        let query = sys::GhosttyTerminalModeConfig {
            mode: value & 0x7fff,
            value: false,
        };
        self.get(sys::GHOSTTY_TERMINAL_DATA_MODE, query)
            .is_some_and(|q| q.value)
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        // SAFETY: plain C structs, zero is a valid bit pattern; `size` is set before each call
        // that reads a sized struct.
        unsafe {
            let mut point: sys::GhosttyPoint = zeroed();
            point.tag = sys::GHOSTTY_POINT_TAG_ACTIVE;
            point.value.coordinate.x = col;
            point.value.coordinate.y = u32::from(row);
            let mut gref: sys::GhosttyGridRef = zeroed();
            gref.size = size_of::<sys::GhosttyGridRef>();
            if sys::ghostty_terminal_grid_ref(self.term.as_ptr(), point, &raw mut gref)
                != sys::GHOSTTY_SUCCESS
            {
                return Cell::blank();
            }
            let mut raw: sys::GhosttyCell = zeroed();
            sys::ghostty_grid_ref_cell(&raw const gref, &raw mut raw);
            let mut cps = vec![0u32; 16];
            let mut n = 0usize;
            let mut rc = sys::ghostty_grid_ref_graphemes(
                &raw const gref,
                cps.as_mut_ptr(),
                cps.len(),
                &raw mut n,
            );
            // A longer grapheme reports its length in `n` and leaves the buffer untouched.
            if rc == sys::GHOSTTY_OUT_OF_SPACE {
                cps = vec![0u32; n];
                rc = sys::ghostty_grid_ref_graphemes(
                    &raw const gref,
                    cps.as_mut_ptr(),
                    cps.len(),
                    &raw mut n,
                );
            }
            let text: String = if rc == sys::GHOSTTY_SUCCESS {
                cps[..n.min(cps.len())]
                    .iter()
                    .filter_map(|&u| char::from_u32(u))
                    .collect()
            } else {
                String::new()
            };
            let mut style: sys::GhosttyStyle = zeroed();
            style.size = size_of::<sys::GhosttyStyle>();
            sys::ghostty_grid_ref_style(&raw const gref, &raw mut style);
            let wide: sys::GhosttyCellWide = cell_data(raw, sys::GHOSTTY_CELL_DATA_WIDE, 0);
            let tag: sys::GhosttyCellContentTag =
                cell_data(raw, sys::GHOSTTY_CELL_DATA_CONTENT_TAG, 0);
            // An erase under a background color stores the color in the cell, not the style.
            let bg = match tag {
                sys::GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE => {
                    Color::Palette(cell_data(raw, sys::GHOSTTY_CELL_DATA_COLOR_PALETTE, 0u8))
                }
                sys::GHOSTTY_CELL_CONTENT_BG_COLOR_RGB => {
                    let rgb: sys::GhosttyColorRgb =
                        cell_data(raw, sys::GHOSTTY_CELL_DATA_COLOR_RGB, zeroed());
                    Color::Rgb(rgb.r, rgb.g, rgb.b)
                }
                _ => style_color(&style.bg_color),
            };
            Cell {
                text: if text.is_empty() { " ".into() } else { text },
                fg: style_color(&style.fg_color),
                bg,
                underline_color: style_color(&style.underline_color),
                attrs: attrs(&style),
                underline: underline(style.underline),
                width: match wide {
                    sys::GHOSTTY_CELL_WIDE_WIDE => CellWidth::Wide,
                    sys::GHOSTTY_CELL_WIDE_SPACER_TAIL => CellWidth::SpacerTail,
                    sys::GHOSTTY_CELL_WIDE_SPACER_HEAD => CellWidth::SpacerHead,
                    _ => CellWidth::Narrow,
                },
            }
        }
    }
}

impl Emulator for GhosttyEmulator {
    type Error = GhosttyError;

    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        // SAFETY: the terminal is live and only this thread uses it. The write_pty callback
        // touches `replies` during this call, and nothing else holds a reference to it.
        unsafe {
            sys::ghostty_terminal_vt_write(self.term.as_ptr(), bytes.as_ptr(), bytes.len());
            std::mem::take(&mut (*self.replies.as_ptr()).0)
        }
    }

    fn size(&self) -> Size {
        let cols = self.get(sys::GHOSTTY_TERMINAL_DATA_COLS, 0u16).unwrap_or(1);
        let rows = self.get(sys::GHOSTTY_TERMINAL_DATA_ROWS, 0u16).unwrap_or(1);
        Size::new(cols, rows)
            .unwrap_or_else(|_| unreachable!("ghostty keeps both dimensions above zero"))
    }

    fn resize(&mut self, size: Size) -> Result<(), GhosttyError> {
        // SAFETY: the terminal is live. Cell pixel sizes only feed size reports, which this
        // emulator does not answer.
        let code = unsafe {
            sys::ghostty_terminal_resize(self.term.as_ptr(), size.cols(), size.rows(), 0, 0)
        };
        check("ghostty_terminal_resize", code)
    }

    fn grid(&self) -> Grid {
        let size = self.size();
        let cells = (0..size.rows())
            .flat_map(|r| (0..size.cols()).map(move |c| (r, c)))
            .map(|(r, c)| self.cell(r, c))
            .collect();
        let cursor = Cursor {
            row: self
                .get(sys::GHOSTTY_TERMINAL_DATA_CURSOR_Y, 0u16)
                .unwrap_or(0),
            col: self
                .get(sys::GHOSTTY_TERMINAL_DATA_CURSOR_X, 0u16)
                .unwrap_or(0),
        };
        Grid::new(size, cells, cursor).unwrap_or_else(|e| unreachable!("ghostty grid: {e}"))
    }

    fn modes(&self) -> Modes {
        let screen = self.get(sys::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, 0);
        Modes {
            screen: if screen == Some(sys::GHOSTTY_TERMINAL_SCREEN_ALTERNATE) {
                Screen::Alternate
            } else {
                Screen::Primary
            },
            cursor_visible: self.mode(25),
            application_cursor_keys: self.mode(1),
            application_keypad: self.mode(66),
            wraparound: self.mode(7),
            mouse_tracking: MouseTracking::from_modes([9, 1000, 1002, 1003].map(|m| self.mode(m))),
            mouse_format: MouseFormat::from_modes([1005, 1006, 1015, 1016].map(|m| self.mode(m))),
            alternate_scroll: self.mode(1007),
            focus_events: self.mode(1004),
            bracketed_paste: self.mode(2004),
            synchronized_output: self.mode(2026),
            grapheme_clustering: self.mode(2027),
            kitty_keyboard_flags: self
                .get(sys::GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS, 0u8)
                .unwrap_or(0),
        }
    }
}

impl Drop for GhosttyEmulator {
    fn drop(&mut self) {
        // SAFETY: both pointers are owned here and freed once; the terminal goes first so no
        // callback can see freed userdata.
        unsafe {
            sys::ghostty_terminal_free(self.term.as_ptr());
            drop(Box::from_raw(self.replies.as_ptr()));
        }
    }
}

/// # Safety
///
/// `T` must be the output type the header documents for `data`.
unsafe fn cell_data<T>(cell: sys::GhosttyCell, data: sys::GhosttyCellData, mut value: T) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe { sys::ghostty_cell_get(cell, data, (&raw mut value).cast::<c_void>()) };
    value
}

fn style_color(c: &sys::GhosttyStyleColor) -> Color {
    // SAFETY: the tag says which union field is live.
    unsafe {
        match c.tag {
            sys::GHOSTTY_STYLE_COLOR_PALETTE => Color::Palette(c.value.palette),
            sys::GHOSTTY_STYLE_COLOR_RGB => {
                let rgb = c.value.rgb;
                Color::Rgb(rgb.r, rgb.g, rgb.b)
            }
            _ => Color::Default,
        }
    }
}

fn attrs(s: &sys::GhosttyStyle) -> Attrs {
    [
        (s.bold, Attrs::BOLD),
        (s.faint, Attrs::FAINT),
        (s.italic, Attrs::ITALIC),
        (s.blink, Attrs::BLINK),
        (s.inverse, Attrs::INVERSE),
        (s.invisible, Attrs::INVISIBLE),
        (s.strikethrough, Attrs::STRIKETHROUGH),
        (s.overline, Attrs::OVERLINE),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(Attrs::NONE, |acc, (_, a)| acc.with(a))
}

fn underline(u: i32) -> Underline {
    match u {
        sys::GHOSTTY_SGR_UNDERLINE_SINGLE => Underline::Single,
        sys::GHOSTTY_SGR_UNDERLINE_DOUBLE => Underline::Double,
        sys::GHOSTTY_SGR_UNDERLINE_CURLY => Underline::Curly,
        sys::GHOSTTY_SGR_UNDERLINE_DOTTED => Underline::Dotted,
        sys::GHOSTTY_SGR_UNDERLINE_DASHED => Underline::Dashed,
        _ => Underline::None,
    }
}

unsafe extern "C" fn on_write_pty(
    _term: sys::GhosttyTerminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() {
        return;
    }
    // SAFETY: the library passes `len` valid bytes; userdata is the live `Replies` installed in
    // `install_effects`, reached only from inside `feed`.
    unsafe {
        let reply = std::slice::from_raw_parts(data, len);
        if emulator::admit_reply(reply) {
            (*userdata.cast::<Replies>()).0.extend_from_slice(reply);
        }
    }
}

unsafe extern "C" fn on_device_attributes(
    _term: sys::GhosttyTerminal,
    _userdata: *mut c_void,
    out: *mut sys::GhosttyDeviceAttributes,
) -> bool {
    if out.is_null() {
        return false;
    }
    let profile = PROFILE.device;
    // SAFETY: the library passes a valid out pointer for the duration of the call.
    let out = unsafe { &mut *out };
    out.primary.conformance_level = profile.conformance_level;
    let n = profile.features.len().min(out.primary.features.len());
    out.primary.features[..n].copy_from_slice(&profile.features[..n]);
    out.primary.num_features = n;
    out.secondary.device_type = profile.device_type;
    out.secondary.firmware_version = profile.firmware_version;
    out.secondary.rom_cartridge = 0;
    out.tertiary.unit_id = profile.unit_id;
    true
}

unsafe extern "C" fn on_xtversion(
    _term: sys::GhosttyTerminal,
    _userdata: *mut c_void,
) -> sys::GhosttyString {
    sys::GhosttyString {
        ptr: PROFILE.version.as_ptr(),
        len: PROFILE.version.len(),
    }
}
