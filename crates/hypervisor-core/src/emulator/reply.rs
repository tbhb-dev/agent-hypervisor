//! One conservative capability profile and terminal query recognition.
//!
//! The emulator answers queries itself, with or without a viewer, because Codex waits 250 ms for
//! DA1 before drawing and Claude Code skips its second probe stage without a reply (RFC-36 run 3).
//! The answers stay conservative: kitty keyboard and graphics probes stay silent.

use super::{Cursor, Size};

/// Device attribute replies: DA1 `CSI ? 62 ; 22 c`, DA2 `CSI > 1 ; 10 ; 0 c`, DA3 unit 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceAttributes {
    /// DA1 conformance level. 62 is a VT220.
    pub conformance_level: u16,
    /// DA1 feature codes. 22 is ANSI color.
    pub features: &'static [u16],
    /// DA2 device type. 1 is a VT220.
    pub device_type: u16,
    /// DA2 firmware version.
    pub firmware_version: u16,
    /// DA3 unit id.
    pub unit_id: u32,
}

/// The fixed values and replies offered to child programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityProfile {
    /// Device attributes.
    pub device: DeviceAttributes,
    /// XTVERSION name.
    pub version: &'static str,
    /// Foreground color for OSC 10.
    pub foreground: &'static str,
    /// Background color for OSC 11.
    pub background: &'static str,
    /// DEC private modes claimed by DECRQM.
    pub supported_modes: &'static [u16],
}

/// The server's deliberately conservative profile, based on RFC-36 runs 3 and 5.
pub const PROFILE: CapabilityProfile = CapabilityProfile {
    device: DeviceAttributes {
        conformance_level: 62,
        features: &[22],
        device_type: 1,
        firmware_version: 10,
        unit_id: 0,
    },
    version: "agent-hypervisor",
    foreground: "rgb:d0d0/d0d0/d0d0",
    background: "rgb:1c1c/1c1c/1c1c",
    supported_modes: &[
        1, 7, 12, 25, 47, 1000, 1002, 1003, 1004, 1006, 1047, 1048, 1049, 2004, 2026,
    ],
};

impl CapabilityProfile {
    /// Answer one complete CSI or OSC query at the current terminal position and size.
    #[must_use]
    pub fn reply(self, query: &[u8], size: Size, cursor: Cursor) -> Option<Vec<u8>> {
        let text = std::str::from_utf8(query).ok()?;
        if let Some(body) = text.strip_prefix("\x1b[") {
            let body = body.strip_suffix(|c: char| c.is_ascii_alphabetic())?;
            let final_byte = text.as_bytes().last().copied()?;
            let out = match (body, final_byte) {
                ("" | "0", b'c') => {
                    let levels = std::iter::once(self.device.conformance_level)
                        .chain(self.device.features.iter().copied())
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(";");
                    format!("\x1b[?{levels}c")
                }
                (">" | ">0", b'c') => format!(
                    "\x1b[>{};{};0c",
                    self.device.device_type, self.device.firmware_version
                ),
                ("=" | "=0", b'c') => format!("\x1bP!|{:08X}\x1b\\", self.device.unit_id),
                (">" | ">0", b'q') => format!("\x1bP>|{}\x1b\\", self.version),
                ("6", b'n') => format!(
                    "\x1b[{};{}R",
                    u32::from(cursor.row) + 1,
                    u32::from(cursor.col) + 1
                ),
                ("5", b'n') => "\x1b[0n".into(),
                ("14", b't') => format!(
                    "\x1b[4;{};{}t",
                    u32::from(size.rows()) * 16,
                    u32::from(size.cols()) * 8
                ),
                ("16", b't') => "\x1b[6;16;8t".into(),
                ("18", b't') => format!("\x1b[8;{};{}t", size.rows(), size.cols()),
                _ => {
                    if let Some(mode) = body.strip_prefix('?').and_then(|s| s.strip_suffix('$'))
                        && final_byte == b'p'
                        && let Ok(mode_number) = mode.parse::<u16>()
                    {
                        let state = if self.supported_modes.contains(&mode_number) {
                            2
                        } else {
                            0
                        };
                        format!("\x1b[?{mode};{state}$y")
                    } else {
                        return None;
                    }
                }
            };
            return Some(out.into_bytes());
        }
        let body = text.strip_prefix("\x1b]")?;
        let (body, ending) = if let Some(body) = body.strip_suffix("\x1b\\") {
            (body, "\x1b\\")
        } else {
            (body.strip_suffix('\x07')?, "\x07")
        };
        let (number, value) = body.split_once(';')?;
        let color = match (number, value) {
            ("10", "?") => self.foreground,
            ("11", "?") => self.background,
            _ => return None,
        };
        Some(format!("\x1b]{number};{color}{ending}").into_bytes())
    }
}

/// Finds complete CSI and OSC sequences across PTY read boundaries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryScanner {
    state: ScanState,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
    Other,
    OtherEscape,
}

impl QueryScanner {
    /// Feed one byte, returning a complete CSI or OSC sequence when one ends.
    pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        use ScanState::{Csi, Escape, Ground, Osc, OscEscape, Other, OtherEscape};
        if self.state == Ground {
            if byte == 0x1b {
                self.state = Escape;
                self.bytes.push(byte);
            }
            return None;
        }
        self.bytes.push(byte);
        let complete = match self.state {
            Ground => false,
            Escape => {
                if byte == 0x1b {
                    self.bytes.clear();
                    self.bytes.push(byte);
                }
                self.state = match byte {
                    b'[' => Csi,
                    b']' => Osc,
                    b'_' | b'P' => Other,
                    0x1b => Escape,
                    _ => Ground,
                };
                false
            }
            Csi => (0x40..=0x7e).contains(&byte),
            Osc => {
                if byte == 0x1b {
                    self.state = OscEscape;
                }
                byte == 0x07
            }
            OscEscape => {
                self.state = if byte == 0x1b { OscEscape } else { Osc };
                byte == b'\\'
            }
            Other => {
                if byte == 0x1b {
                    self.state = OtherEscape;
                }
                false
            }
            OtherEscape => {
                self.state = if byte == b'\\' {
                    Ground
                } else if byte == 0x1b {
                    OtherEscape
                } else {
                    Other
                };
                false
            }
        };
        if complete {
            self.state = Ground;
            return Some(std::mem::take(&mut self.bytes));
        }
        if self.state == Ground || self.bytes.len() > 256 {
            self.state = Ground;
            self.bytes.clear();
        }
        None
    }
}

/// Filters terminal-generated answers out of one viewer's input stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewerInputFilter {
    pending: Vec<u8>,
}

impl ViewerInputFilter {
    /// Forward ordinary input, retaining incomplete escape sequences across chunks.
    pub fn filter(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut forwarded = Vec::new();
        for &byte in bytes {
            if self.pending.is_empty() {
                if byte == 0x1b {
                    self.pending.push(byte);
                } else {
                    forwarded.push(byte);
                }
                continue;
            }
            self.pending.push(byte);
            let kind = self.pending.get(1).copied();
            let complete = match kind {
                Some(b'[') => self.pending.len() > 2 && (0x40..=0x7e).contains(&byte),
                Some(b']') => byte == 0x07 || self.pending.ends_with(b"\x1b\\"),
                Some(b'P') => self.pending.ends_with(b"\x1b\\"),
                Some(_) => true,
                None => false,
            };
            if complete || self.pending.len() > 256 {
                if !is_terminal_reply(&self.pending) {
                    forwarded.extend_from_slice(&self.pending);
                }
                self.pending.clear();
            }
        }
        forwarded
    }

    /// Whether an escape sequence is waiting for more bytes.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Forward an unfinished sequence after the input ambiguity deadline.
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

/// Whether a complete escape sequence is a terminal-generated answer, not application input.
#[must_use]
pub fn is_terminal_reply(bytes: &[u8]) -> bool {
    if let Some(body) = bytes.strip_prefix(b"\x1b[") {
        let Some((&last, middle)) = body.split_last() else {
            return false;
        };
        return match last {
            b'c' => {
                middle.starts_with(b"?") || middle.starts_with(b">") || middle.starts_with(b"=")
            }
            b'R' => {
                let coordinates = middle.strip_prefix(b"?").unwrap_or(middle);
                !coordinates.is_empty()
                    && coordinates.iter().all(|b| b.is_ascii_digit() || *b == b';')
            }
            b'y' => middle.ends_with(b"$"),
            b'u' => {
                middle.starts_with(b"?")
                    && middle[1..].iter().all(|b| b.is_ascii_digit() || *b == b';')
            }
            _ => false,
        };
    }
    if let Some(body) = bytes.strip_prefix(b"\x1b]") {
        return [b"4;".as_slice(), b"10;", b"11;"]
            .iter()
            .any(|prefix| body.starts_with(prefix));
    }
    bytes.starts_with(b"\x1bP!|") || bytes.starts_with(b"\x1bP>|")
}

/// Whether one reply the emulator produced may go to the application.
///
/// Rejects the kitty keyboard protocol reply `CSI ? <flags> u`. Answering that query is what
/// claims the protocol: Claude Code switched to it when answered, and a raw `0x03` then stopped
/// acting as Ctrl-C (RFC-36 run 3). Every other reply passes.
#[must_use]
pub fn admit_reply(reply: &[u8]) -> bool {
    !is_kitty_keyboard_reply(reply)
}

fn is_kitty_keyboard_reply(reply: &[u8]) -> bool {
    reply
        .strip_prefix(b"\x1b[?")
        .and_then(|rest| rest.strip_suffix(b"u"))
        .is_some_and(|flags| !flags.is_empty() && flags.iter().all(u8::is_ascii_digit))
}

#[cfg(test)]
mod tests {
    use super::{PROFILE, QueryScanner, ViewerInputFilter, admit_reply};
    use crate::emulator::{Cursor, Size};
    use proptest::prelude::*;

    #[test]
    fn kitty_keyboard_replies_are_dropped() {
        assert!(!admit_reply(b"\x1b[?0u"));
        assert!(!admit_reply(b"\x1b[?31u"));
    }

    #[test]
    fn other_replies_pass() {
        assert!(admit_reply(b"\x1b[?62;22c"));
        assert!(admit_reply(b"\x1b[1;1R"));
        assert!(admit_reply(b"\x1b[?2026;2$y"));
        assert!(admit_reply(b"\x1b[?u"));
        assert!(admit_reply(b"\x1b[?1;2u"));
    }

    #[test]
    fn fixed_profile_answers_queries_and_leaves_kitty_silent() {
        let size = Size::new(80, 24).unwrap();
        let cursor = Cursor { row: 2, col: 4 };
        for (query, answer) in [
            (&b"\x1b[c"[..], &b"\x1b[?62;22c"[..]),
            (b"\x1b[>c", b"\x1b[>1;10;0c"),
            (b"\x1b[>q", b"\x1bP>|agent-hypervisor\x1b\\"),
            (b"\x1b[?2026$p", b"\x1b[?2026;2$y"),
            (b"\x1b[?2027$p", b"\x1b[?2027;0$y"),
            (b"\x1b[6n", b"\x1b[3;5R"),
            (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"),
            (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\"),
            (b"\x1b[18t", b"\x1b[8;24;80t"),
        ] {
            assert_eq!(
                PROFILE.reply(query, size, cursor).as_deref(),
                Some(answer),
                "{query:?}"
            );
        }
        assert_eq!(PROFILE.reply(b"\x1b[?u", size, cursor), None);
        assert_eq!(PROFILE.reply(b"\x1b_Ga=q\x1b\\", size, cursor), None);
    }

    #[test]
    fn scanner_preserves_codex_and_herdr_query_order() {
        let mut scanner = QueryScanner::default();
        let mut seen = Vec::new();
        for &byte in b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b[c" {
            if let Some(query) = scanner.push(byte) {
                seen.push(query);
            }
        }
        assert_eq!(
            seen,
            [
                b"\x1b[6n".as_slice(),
                b"\x1b]10;?\x1b\\",
                b"\x1b]11;?\x1b\\",
                b"\x1b[?u",
                b"\x1b[c"
            ]
        );
        let mut scanner = QueryScanner::default();
        let mut seen = Vec::new();
        for &byte in b"\x1b]11;?\x07\x1b[6n" {
            if let Some(query) = scanner.push(byte) {
                seen.push(query);
            }
        }
        assert_eq!(seen, [b"\x1b]11;?\x07".as_slice(), b"\x1b[6n"]);
    }

    #[test]
    fn input_filter_drops_replies_across_reads_but_passes_keys() {
        let mut filter = ViewerInputFilter::default();
        assert!(filter.filter(b"\x1b[?62").is_empty());
        assert_eq!(filter.filter(b";22cabc\x1b[A"), b"abc\x1b[A");
        assert!(
            filter
                .filter(b"\x1b[3;5R\x1b[?2026;2$y\x1b]11;rgb:1c1c/1c1c/1c1c\x07")
                .is_empty()
        );
        assert!(
            filter
                .filter(b"\x1bP>|agent-hypervisor\x1b\\\x1b[?0u")
                .is_empty()
        );
    }

    #[test]
    fn every_listed_terminal_reply_is_filtered() {
        for reply in [
            &b"\x1b[?62;22c"[..],
            b"\x1b[>1;10;0c",
            b"\x1bP!|00000000\x1b\\",
            b"\x1bP>|agent-hypervisor\x1b\\",
            b"\x1b[1;1R",
            b"\x1b[?1;1;1R",
            b"\x1b[?2026;2$y",
            b"\x1b[2026;0$y",
            b"\x1b]4;1;rgb:ffff/0000/0000\x07",
            b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\",
            b"\x1b]11;rgb:1c1c/1c1c/1c1c\x07",
            b"\x1b[?0u",
        ] {
            assert!(
                ViewerInputFilter::default().filter(reply).is_empty(),
                "{reply:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn query_order_survives_arbitrary_batches(
            choices in proptest::collection::vec(0usize..4, 0..40),
        ) {
            let queries: [&[u8]; 4] = [b"\x1b[c", b"\x1b[6n", b"\x1b]11;?\x07", b"\x1b[?2026$p"];
            let mut scanner = QueryScanner::default();
            let mut seen = Vec::new();
            for choice in &choices {
                for &byte in queries[*choice] {
                    if let Some(query) = scanner.push(byte) { seen.push(query); }
                }
            }
            let expected: Vec<Vec<u8>> = choices.iter().map(|&i| queries[i].to_vec()).collect();
            prop_assert_eq!(seen, expected);
        }

        #[test]
        fn split_kitty_reply_is_never_forwarded(flags in 0u8..=31, cut in 0usize..8) {
            let reply = format!("\x1b[?{flags}u");
            let at = cut.min(reply.len());
            let mut filter = ViewerInputFilter::default();
            prop_assert!(filter.filter(&reply.as_bytes()[..at]).is_empty());
            prop_assert!(filter.filter(&reply.as_bytes()[at..]).is_empty());
        }

        #[test]
        fn every_flag_value_is_dropped(flags in 0u8..=31) {
            let reply = format!("\x1b[?{flags}u");
            prop_assert!(!admit_reply(reply.as_bytes()));
        }

        #[test]
        fn replies_not_ending_in_u_pass(body in "[0-9;?>$]*", last in "[A-Za-tv-z]") {
            let reply = format!("\x1b[{body}{last}");
            prop_assert!(admit_reply(reply.as_bytes()));
        }
    }
}
