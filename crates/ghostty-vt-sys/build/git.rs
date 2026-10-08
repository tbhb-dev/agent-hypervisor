//! The one way the build script runs git.
//!
//! A git hook exports `GIT_DIR`, `GIT_INDEX_FILE`, and the other repository-local variables, and
//! cargo passes them on to this build script when a hook runs clippy or the tests. A git command
//! that inherits them acts on the enclosing repository instead of the directory given with `-C`:
//! a pre-commit hook once shallow-fetched Ghostty into this repository's object store and then
//! tried to check Ghostty's tree out over the worktree (RFC-36 run 9). [`git`] removes every
//! variable `git rev-parse --local-env-vars` lists, so `-C` decides the repository.

use std::path::Path;
use std::process::Command;

/// The variables `git rev-parse --local-env-vars` prints (git 2.55.0).
pub const LOCAL_ENV_VARS: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `git -C dir` with none of the repository-local variables inherited.
pub fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for var in LOCAL_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}
