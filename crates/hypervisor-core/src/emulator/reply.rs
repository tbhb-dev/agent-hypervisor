//! Which terminal replies reach the application, and what the device attribute replies say.
//!
//! The emulator answers queries itself, with or without a viewer, because Codex waits 250 ms for
//! DA1 before drawing and Claude Code skips its second probe stage without a reply (RFC-36 run 3).
//! The answers stay conservative. Run 10 replaces these constants with the full capability
//! profile; this module holds only what the emulator needs to answer on its own.

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

/// The device attributes the emulator reports: a VT220 with ANSI color, the profile RFC-36
/// run 3's capture tool answered with.
pub const DEVICE_ATTRIBUTES: DeviceAttributes = DeviceAttributes {
    conformance_level: 62,
    features: &[22],
    device_type: 1,
    firmware_version: 10,
    unit_id: 0,
};

/// The name the emulator reports to XTVERSION (`CSI > q`).
pub const XTVERSION_NAME: &str = "agent-hypervisor";

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
    use super::admit_reply;
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

    proptest! {
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
