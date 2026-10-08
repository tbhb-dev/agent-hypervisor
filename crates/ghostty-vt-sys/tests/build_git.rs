//! The build script's git commands ignore the repository a git hook points them at.

#[path = "../build/git.rs"]
mod git;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD: &str = "GHOSTTY_VT_SYS_GIT_ENV_CHILD";

#[test]
fn the_variable_list_covers_what_git_calls_repository_local() {
    let out = Command::new("git")
        .args(["rev-parse", "--local-env-vars"])
        .output()
        .expect("run git rev-parse --local-env-vars");
    assert!(out.status.success());
    let listed = String::from_utf8(out.stdout).expect("utf-8");
    for var in listed.lines() {
        assert!(
            git::LOCAL_ENV_VARS.contains(&var),
            "{var} is missing from LOCAL_ENV_VARS"
        );
    }
}

#[test]
fn the_build_script_runs_git_only_through_the_helper() {
    let build = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))
        .expect("read build.rs");
    assert!(!build.contains("Command::new(\"git\")"));
}

/// Runs `git -C <dir> init` the way the build script does, from a process whose environment
/// points `GIT_DIR` at another repository, as a pre-commit hook's does.
#[test]
fn git_init_lands_in_the_named_directory_under_a_hook_environment() {
    if let Some(dir) = std::env::var_os(CHILD) {
        let status = git::git(Path::new(&dir))
            .args(["init", "-q"])
            .status()
            .expect("run git init");
        assert!(status.success());
        return;
    }
    let root = PathBuf::from(format!(
        "/private/tmp/ghostty-git-env-{}",
        std::process::id()
    ));
    let (enclosing, named) = (root.join("enclosing"), root.join("named"));
    fs::create_dir_all(&enclosing).expect("create the enclosing directory");
    fs::create_dir_all(&named).expect("create the named directory");
    let init = Command::new("git")
        .arg("-C")
        .arg(&enclosing)
        .args(["init", "-q"])
        .status()
        .expect("init the enclosing repository");
    assert!(init.success());

    let me = std::env::current_exe().expect("the test binary");
    let status = Command::new(me)
        .args([
            "--exact",
            "git_init_lands_in_the_named_directory_under_a_hook_environment",
        ])
        .env(CHILD, &named)
        .env("GIT_DIR", enclosing.join(".git"))
        .env("GIT_INDEX_FILE", enclosing.join(".git/index"))
        .env("GIT_WORK_TREE", &enclosing)
        .status()
        .expect("rerun this test as the child");
    let created = named.join(".git/HEAD").is_file();
    fs::remove_dir_all(&root).expect("remove the temporary directories");
    assert!(status.success());
    assert!(created, "git init went to the enclosing repository");
}
