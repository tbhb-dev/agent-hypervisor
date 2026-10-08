//! Control sequences an emulator's own parser leaves unanswered or untracked.
//!
//! `alacritty_terminal` hard-codes its own device attributes, ignores DA3 and XTVERSION, and keeps
//! no state for modes 2026 and 2027. Its shell crate runs a second parser beside it and passes
//! each CSI it sees to [`csi_effects`], which says what to answer and which mode changed. The
//! answers come from [`PROFILE`], the same values ghostty-vt
//! answers with.

use super::reply::PROFILE;

/// The private modes [`csi_effects`] reports changes to: 2026, synchronized output, and 2027,
/// grapheme clustering.
pub const TRACKED_MODES: [u16; 2] = [2026, 2027];

/// What one CSI asks of the emulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CsiEffect {
    /// Bytes owed to the application.
    Reply(Vec<u8>),
    /// A mode in [`TRACKED_MODES`] was set or reset.
    Mode {
        /// The DEC private mode number.
        mode: u16,
        /// Whether it was set.
        set: bool,
    },
}

/// The effects of one CSI, given its intermediate bytes (including a `?`, `>`, or `=` prefix),
/// the first value of each parameter, and its final character.
#[must_use]
pub fn csi_effects(intermediates: &[u8], params: &[u16], action: char) -> Vec<CsiEffect> {
    let first = params.first().copied().unwrap_or(0);
    let reply = |bytes: String| vec![CsiEffect::Reply(bytes.into_bytes())];
    let da = PROFILE.device;
    match (intermediates, action) {
        ([], 'c') if first == 0 => {
            let levels: Vec<String> = std::iter::once(da.conformance_level)
                .chain(da.features.iter().copied())
                .map(|n| n.to_string())
                .collect();
            reply(format!("\x1b[?{}c", levels.join(";")))
        }
        (b">", 'c') if first == 0 => reply(format!(
            "\x1b[>{};{};0c",
            da.device_type, da.firmware_version
        )),
        (b"=", 'c') if first == 0 => reply(format!("\x1bP!|{:08X}\x1b\\", da.unit_id)),
        (b">", 'q') if first == 0 => reply(format!("\x1bP>|{}\x1b\\", PROFILE.version)),
        (b"?", 'h' | 'l') => params
            .iter()
            .filter(|m| TRACKED_MODES.contains(m))
            .map(|&mode| CsiEffect::Mode {
                mode,
                set: action == 'h',
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CsiEffect, csi_effects};
    use crate::emulator::admit_reply;
    use proptest::prelude::*;

    fn reply(intermediates: &[u8], params: &[u16], action: char) -> String {
        match csi_effects(intermediates, params, action).as_slice() {
            [CsiEffect::Reply(r)] => String::from_utf8(r.clone()).unwrap(),
            other => panic!("expected one reply, got {other:?}"),
        }
    }

    #[test]
    fn identity_queries_are_answered_from_the_profile() {
        assert_eq!(reply(b"", &[], 'c'), "\x1b[?62;22c");
        assert_eq!(reply(b"", &[0], 'c'), "\x1b[?62;22c");
        assert_eq!(reply(b">", &[], 'c'), "\x1b[>1;10;0c");
        assert_eq!(reply(b"=", &[0], 'c'), "\x1bP!|00000000\x1b\\");
        assert_eq!(reply(b">", &[], 'q'), "\x1bP>|agent-hypervisor\x1b\\");
    }

    #[test]
    fn tracked_modes_are_reported_and_others_ignored() {
        assert_eq!(
            csi_effects(b"?", &[1049, 2026, 2027], 'h'),
            vec![
                CsiEffect::Mode {
                    mode: 2026,
                    set: true
                },
                CsiEffect::Mode {
                    mode: 2027,
                    set: true
                },
            ]
        );
        assert_eq!(
            csi_effects(b"?", &[2026], 'l'),
            vec![CsiEffect::Mode {
                mode: 2026,
                set: false
            }]
        );
        assert!(csi_effects(b"", &[2026], 'h').is_empty());
        assert!(csi_effects(b"", &[1], 'c').is_empty());
        assert!(csi_effects(b"", &[5], 'n').is_empty());
    }

    proptest! {
        #[test]
        fn every_reply_is_admitted(
            inter in prop::sample::select(vec![&b""[..], b"?", b">", b"=", b" ", b"$"]),
            params in prop::collection::vec(any::<u16>(), 0..4),
            action in prop::char::range('@', '~'),
        ) {
            for effect in csi_effects(inter, &params, action) {
                if let CsiEffect::Reply(r) = effect {
                    prop_assert!(admit_reply(&r));
                }
            }
        }
    }
}
