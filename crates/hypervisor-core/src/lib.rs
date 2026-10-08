//! Pure core of the agent hypervisor.
//!
//! Decisions and data transformations live here as pure functions. Shell crates such as
//! `hypervisord` read the world, call into this crate with plain values, and act on the result.
#![forbid(
    unsafe_code,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

pub mod attenuation;
pub mod channel;
pub mod emulator;
pub mod screen;
pub mod session;
pub mod state;

/// Formats the line a binary prints to report its name and version.
#[must_use]
pub fn version_line(name: &str, version: &str) -> String {
    format!("{name} {version}")
}

#[cfg(test)]
mod tests {
    use super::version_line;
    use proptest::prelude::*;

    #[test]
    fn joins_name_and_version_with_one_space() {
        assert_eq!(version_line("hypervisord", "0.1.0"), "hypervisord 0.1.0");
    }

    proptest! {
        #[test]
        fn line_is_name_then_space_then_version(name in "\\PC*", version in "\\PC*") {
            let line = version_line(&name, &version);
            let rest = line.strip_prefix(name.as_str()).and_then(|rest| rest.strip_prefix(' '));
            prop_assert_eq!(rest, Some(version.as_str()));
        }
    }
}
