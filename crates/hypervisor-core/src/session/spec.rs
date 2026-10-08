//! The spawn spec: what a session runs and how it starts.

use std::fmt;
use std::path::PathBuf;

use crate::emulator::Size;

/// What a session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionKind {
    /// An agent harness such as Claude Code or Codex.
    Agent,
    /// An interactive shell.
    Shell,
}

/// How a session is started.
///
/// `env` is the child's whole environment: nothing is inherited from the spawning process. Its
/// values never appear in `Debug` output, because credential placeholders arrive through it
/// (RFC-37 run 15) and run 24 injects credentials at spawn.
#[derive(Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// The program, looked up on the child's `PATH` when it has no slash.
    pub command: String,
    /// Arguments after the program name.
    pub args: Vec<String>,
    /// Environment variables as name and value pairs.
    pub env: Vec<(String, String)>,
    /// The working directory, absolute. `None` keeps the spawner's.
    pub cwd: Option<PathBuf>,
    /// A user to run as. Only a container driver can honour it; see [`SpawnError::OtherUser`].
    pub user: Option<String>,
    /// The terminal size, applied before the program starts.
    pub size: Size,
    /// What the session runs.
    pub kind: SessionKind,
}

impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("SpawnSpec")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env_names", &names)
            .field("cwd", &self.cwd)
            .field("user", &self.user)
            .field("size", &self.size)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Why a [`SpawnSpec`] cannot be spawned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// The command is empty.
    EmptyCommand,
    /// A command, argument, or environment entry holds a NUL byte, which `exec` cannot pass.
    Nul {
        /// Which field: `command`, `args`, or `env`.
        field: &'static str,
    },
    /// An environment name is empty or contains `=`.
    EnvName {
        /// The entry's position in `env`.
        index: usize,
    },
    /// The working directory is relative.
    RelativeCwd,
    /// Another user was asked for. The host PTY backend runs as the daemon's own user; only the
    /// named driver can start a process as someone else (RFC-36 run 1).
    OtherUser {
        /// The driver that supports `user`.
        driver: &'static str,
    },
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => f.write_str("the command is empty"),
            Self::Nul { field } => write!(f, "the {field} field holds a NUL byte"),
            Self::EnvName { index } => write!(f, "environment entry {index} has an invalid name"),
            Self::RelativeCwd => f.write_str("the working directory is not absolute"),
            Self::OtherUser { driver } => {
                write!(f, "running as another user needs the {driver} driver")
            }
        }
    }
}

impl std::error::Error for SpawnError {}

impl SpawnSpec {
    /// A spec with no arguments, an empty environment, and no working directory or user.
    #[must_use]
    pub fn new(command: impl Into<String>, size: Size, kind: SessionKind) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            user: None,
            size,
            kind,
        }
    }

    /// Checks that the host PTY backend can spawn the spec. The size needs no check, because
    /// [`Size`] already rejects a zero dimension.
    ///
    /// # Errors
    ///
    /// The first problem found, in field order.
    pub fn validate(&self) -> Result<(), SpawnError> {
        if self.command.is_empty() {
            return Err(SpawnError::EmptyCommand);
        }
        if self.command.contains('\0') {
            return Err(SpawnError::Nul { field: "command" });
        }
        if self.args.iter().any(|a| a.contains('\0')) {
            return Err(SpawnError::Nul { field: "args" });
        }
        for (index, (name, value)) in self.env.iter().enumerate() {
            if name.contains('\0') || value.contains('\0') {
                return Err(SpawnError::Nul { field: "env" });
            }
            if name.is_empty() || name.contains('=') {
                return Err(SpawnError::EnvName { index });
            }
        }
        if self.cwd.as_ref().is_some_and(|cwd| !cwd.is_absolute()) {
            return Err(SpawnError::RelativeCwd);
        }
        if self.user.is_some() {
            return Err(SpawnError::OtherUser {
                driver: "container",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionKind, SpawnError, SpawnSpec};
    use crate::emulator::Size;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn spec() -> SpawnSpec {
        SpawnSpec::new("/bin/sh", Size::new(80, 24).unwrap(), SessionKind::Shell)
    }

    #[test]
    fn a_plain_spec_is_valid() {
        let mut s = spec();
        s.args = vec!["-c".into(), "true".into()];
        s.env = vec![("PATH".into(), "/bin".into())];
        s.cwd = Some(PathBuf::from("/"));
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn each_problem_is_named() {
        let mut s = spec();
        s.command.clear();
        assert_eq!(s.validate(), Err(SpawnError::EmptyCommand));

        let mut s = spec();
        s.args = vec!["a\0b".into()];
        assert_eq!(s.validate(), Err(SpawnError::Nul { field: "args" }));

        let mut s = spec();
        s.env = vec![("OK".into(), "1".into()), ("A=B".into(), "2".into())];
        assert_eq!(s.validate(), Err(SpawnError::EnvName { index: 1 }));

        let mut s = spec();
        s.env = vec![(String::new(), "1".into())];
        assert_eq!(s.validate(), Err(SpawnError::EnvName { index: 0 }));

        let mut s = spec();
        s.cwd = Some(PathBuf::from("relative"));
        assert_eq!(s.validate(), Err(SpawnError::RelativeCwd));

        let mut s = spec();
        s.user = Some("agent".into());
        assert_eq!(
            s.validate(),
            Err(SpawnError::OtherUser {
                driver: "container"
            })
        );
    }

    proptest! {
        #[test]
        fn debug_never_shows_an_environment_value(name in "[A-Z_]{1,8}", value in "[a-z0-9]{12,24}") {
            let mut s = spec();
            s.env = vec![(name.clone(), value.clone())];
            let shown = format!("{s:?}");
            prop_assert!(shown.contains(&name));
            prop_assert!(!shown.contains(&value));
        }

        #[test]
        fn a_nul_anywhere_in_the_command_is_rejected(prefix in "[a-z/]{0,8}", suffix in "[a-z]{0,8}") {
            let mut s = spec();
            s.command = format!("{prefix}\0{suffix}");
            prop_assert_eq!(s.validate(), Err(SpawnError::Nul { field: "command" }));
        }
    }
}
