//! Shell for the repository checks. It reads `cargo metadata` and commit message files, hands
//! plain values to `xtask-core`, prints the result, and sets the exit status.
//!
//! Usage: `xtask boundary` or `xtask commit-msg <message file>`.

use std::path::Path;
use std::process::{Command, ExitCode};

use serde_json::Value;
use xtask_core::boundary::{self, Dependency, DependencyKind, Package};
use xtask_core::commit_msg::{self, Verdict};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["boundary"] => run_boundary(),
        ["commit-msg", path] => run_commit_msg(path),
        _ => Err("usage: xtask boundary | xtask commit-msg <message file>".to_owned()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

fn run_boundary() -> Result<bool, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()
        .map_err(|e| format!("running cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("parsing cargo metadata: {e}"))?;
    let packages = packages(&metadata)?;
    let allow = allowlist(&metadata);
    let found = boundary::violations(&packages, &allow);
    for violation in &found {
        eprintln!("boundary: {violation}");
    }
    let cores = packages
        .iter()
        .filter(|p| boundary::is_core(&p.name))
        .count();
    if found.is_empty() {
        println!(
            "boundary ok: {cores} core crate(s), allowlist {allow:?}, {} package(s) checked",
            packages.len()
        );
    }
    Ok(found.is_empty())
}

fn packages(metadata: &Value) -> Result<Vec<Package>, String> {
    let list = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages array")?;
    list.iter()
        .map(|package| {
            let name = package["name"].as_str().ok_or("package without a name")?;
            let manifest = package["manifest_path"]
                .as_str()
                .ok_or("package without a manifest_path")?;
            let has_clippy_config = Path::new(manifest)
                .parent()
                .is_some_and(|dir| dir.join("clippy.toml").is_file());
            let dependencies = package["dependencies"]
                .as_array()
                .map(|deps| deps.iter().filter_map(dependency).collect())
                .unwrap_or_default();
            Ok(Package {
                name: name.to_owned(),
                has_clippy_config,
                dependencies,
            })
        })
        .collect()
}

fn dependency(dep: &Value) -> Option<Dependency> {
    let kind = match dep["kind"].as_str() {
        None => DependencyKind::Normal,
        Some("dev") => DependencyKind::Development,
        Some(_) => DependencyKind::Build,
    };
    Some(Dependency {
        name: dep["name"].as_str()?.to_owned(),
        kind,
    })
}

fn allowlist(metadata: &Value) -> Vec<String> {
    metadata["metadata"]["boundary"]["allow"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn run_commit_msg(path: &str) -> Result<bool, String> {
    let message = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    match commit_msg::check(&message) {
        Verdict::Exempt => {
            println!("wip commit exempt");
            Ok(true)
        }
        Verdict::Checked(problems) if problems.is_empty() => {
            println!("commit message ok");
            Ok(true)
        }
        Verdict::Checked(problems) => {
            for problem in &problems {
                eprintln!("commit-msg: {problem}");
            }
            Ok(false)
        }
    }
}
