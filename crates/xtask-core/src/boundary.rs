//! The functional core boundary.
//!
//! A crate whose name ends in `-core` is pure. Its normal dependencies must be other `-core`
//! crates or crates on the workspace allowlist. Its `clippy.toml` must list every path in
//! [`REQUIRED_BANS`], and its crate root must forbid `unsafe_code` and the three disallowed
//! lints, so neither an emptied config nor an `allow` attribute can switch the I/O ban off.
//! Development and build dependencies are not checked: they never reach the library.

use std::fmt;

/// The suffix that marks a pure crate.
pub const CORE_SUFFIX: &str = "-core";

/// The lints every core crate root must name in one `#![forbid(...)]` attribute.
pub const REQUIRED_FORBID: [&str; 4] = [
    "unsafe_code",
    "clippy::disallowed_methods",
    "clippy::disallowed_types",
    "clippy::disallowed_macros",
];

/// The paths every core `clippy.toml` must ban, as `path = "..."` entries.
pub const REQUIRED_BANS: [&str; 79] = [
    "std::fs::read",
    "std::fs::read_to_string",
    "std::fs::read_dir",
    "std::fs::read_link",
    "std::fs::write",
    "std::fs::copy",
    "std::fs::rename",
    "std::fs::create_dir",
    "std::fs::create_dir_all",
    "std::fs::remove_file",
    "std::fs::remove_dir",
    "std::fs::remove_dir_all",
    "std::fs::metadata",
    "std::fs::symlink_metadata",
    "std::fs::canonicalize",
    "std::fs::exists",
    "std::fs::hard_link",
    "std::fs::set_permissions",
    "std::path::Path::exists",
    "std::path::Path::metadata",
    "std::path::Path::read_dir",
    "std::path::Path::canonicalize",
    "std::path::Path::is_file",
    "std::path::Path::is_dir",
    "std::path::Path::try_exists",
    "std::path::Path::symlink_metadata",
    "std::path::Path::read_link",
    "std::net::ToSocketAddrs::to_socket_addrs",
    "std::net::TcpStream::connect",
    "std::net::TcpListener::bind",
    "std::net::UdpSocket::bind",
    "std::process::exit",
    "std::process::abort",
    "std::process::id",
    "std::env::var",
    "std::env::var_os",
    "std::env::vars",
    "std::env::vars_os",
    "std::env::args",
    "std::env::args_os",
    "std::env::set_var",
    "std::env::remove_var",
    "std::env::current_dir",
    "std::env::set_current_dir",
    "std::env::current_exe",
    "std::env::home_dir",
    "std::env::temp_dir",
    "std::time::SystemTime::now",
    "std::time::Instant::now",
    "std::time::SystemTime::elapsed",
    "std::time::Instant::elapsed",
    "std::thread::spawn",
    "std::thread::scope",
    "std::thread::sleep",
    "std::thread::Builder::spawn",
    "std::io::stdin",
    "std::io::stdout",
    "std::io::stderr",
    "std::fs::File",
    "std::fs::OpenOptions",
    "std::fs::ReadDir",
    "std::net::TcpStream",
    "std::net::TcpListener",
    "std::net::UdpSocket",
    "std::os::unix::net::UnixStream",
    "std::os::unix::net::UnixListener",
    "std::os::unix::net::UnixDatagram",
    "std::process::Command",
    "std::process::Child",
    "std::thread::Builder",
    "std::thread::JoinHandle",
    "std::io::Stdin",
    "std::io::Stdout",
    "std::io::Stderr",
    "std::print",
    "std::println",
    "std::eprint",
    "std::eprintln",
    "std::dbg",
];

/// How a package depends on another, as `cargo metadata` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Normal,
    Development,
    Build,
}

/// One declared dependency of a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub kind: DependencyKind,
}

/// One workspace package and the facts the boundary decision needs about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    /// The text of the `clippy.toml` beside the manifest, or `None` when there is none.
    pub clippy_config: Option<String>,
    /// The text of the library crate root, or `None` when the package has no library target.
    pub lib_root: Option<String>,
    pub dependencies: Vec<Dependency>,
}

/// A breach of the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A core crate has a normal dependency that is neither a core crate nor allowlisted.
    Dependency { package: String, dependency: String },
    /// A core crate has no `clippy.toml` beside its manifest, so the I/O ban does not apply.
    MissingClippyConfig { package: String },
    /// A core crate's `clippy.toml` does not ban a required path.
    MissingBan { package: String, path: String },
    /// A core crate has no library root to carry the `forbid` attribute.
    MissingLibRoot { package: String },
    /// A core crate root's `#![forbid(...)]` does not name a required lint.
    MissingForbid { package: String, lint: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dependency {
                package,
                dependency,
            } => write!(
                f,
                "{package}: normal dependency `{dependency}` is not a {CORE_SUFFIX} crate or on [workspace.metadata.boundary] allow"
            ),
            Self::MissingClippyConfig { package } => write!(
                f,
                "{package}: no clippy.toml beside Cargo.toml, so the core I/O ban does not apply"
            ),
            Self::MissingBan { package, path } => {
                write!(f, "{package}: clippy.toml does not ban `{path}`")
            }
            Self::MissingLibRoot { package } => {
                write!(f, "{package}: no library target to carry #![forbid(...)]")
            }
            Self::MissingForbid { package, lint } => {
                write!(
                    f,
                    "{package}: crate root does not have `{lint}` in #![forbid(...)]"
                )
            }
        }
    }
}

/// Reports whether a crate name marks a pure crate.
#[must_use]
pub fn is_core(name: &str) -> bool {
    name.ends_with(CORE_SUFFIX)
}

/// Returns the `path = "..."` values on uncommented lines of a `clippy.toml`.
#[must_use]
pub fn banned_paths(config: &str) -> Vec<&str> {
    config
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(|line| line.split("path = \"").skip(1))
        .filter_map(|rest| rest.split_once('"').map(|(path, _)| path))
        .collect()
}

/// Returns the lints named in every `#![forbid(...)]` attribute of a crate root, ignoring
/// line and nested block comments, and string and char literals.
#[must_use]
pub fn forbidden_lints(lib_root: &str) -> Vec<String> {
    let uncommented = strip_comments(lib_root);
    let code: String = uncommented.chars().filter(|c| !c.is_whitespace()).collect();
    code.split("#![forbid(")
        .skip(1)
        .filter_map(|rest| rest.split_once(")]").map(|(list, _)| list))
        .flat_map(|list| list.split(','))
        .filter(|lint| !lint.is_empty())
        .map(str::to_owned)
        .collect()
}

fn strip_comments(source: &str) -> String {
    let mut chars = source.chars().peekable();
    let mut code = String::with_capacity(source.len());
    let mut block_depth = 0;
    let mut line_comment = false;
    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
                code.push(ch);
            }
        } else if block_depth > 0 {
            match (ch, chars.peek().copied()) {
                ('/', Some('*')) => {
                    chars.next();
                    block_depth += 1;
                }
                ('*', Some('/')) => {
                    chars.next();
                    block_depth -= 1;
                }
                _ => {}
            }
        } else {
            match (ch, chars.peek().copied()) {
                ('/', Some('/')) => {
                    chars.next();
                    line_comment = true;
                    code.push(' ');
                }
                ('/', Some('*')) => {
                    chars.next();
                    block_depth = 1;
                    code.push(' ');
                }
                ('r', _) if skip_raw_string(&mut chars) => code.push(' '),
                ('"', _) => {
                    skip_string(&mut chars);
                    code.push(' ');
                }
                ('\'', _) if skip_char(&mut chars) => code.push(' '),
                _ => code.push(ch),
            }
        }
    }
    code
}

fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            '"' => break,
            _ => {}
        }
    }
}

fn skip_raw_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut lookahead = chars.clone();
    let mut hashes = 0;
    while lookahead.next_if_eq(&'#').is_some() {
        hashes += 1;
    }
    if lookahead.next_if_eq(&'"').is_none() {
        return false;
    }
    *chars = lookahead;
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut ending = chars.clone();
            if (0..hashes).all(|_| ending.next_if_eq(&'#').is_some()) {
                *chars = ending;
                break;
            }
        }
    }
    true
}

fn skip_char(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut lookahead = chars.clone();
    let Some(first) = lookahead.next() else {
        return false;
    };
    if first == '\\' {
        let Some(escape) = lookahead.next() else {
            return false;
        };
        match escape {
            'x' => {
                lookahead.next();
                lookahead.next();
            }
            'u' if lookahead.next_if_eq(&'{').is_some() => {
                for ch in lookahead.by_ref() {
                    if ch == '}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if lookahead.next_if_eq(&'\'').is_none() {
        return false;
    }
    *chars = lookahead;
    true
}

/// Returns every boundary violation among `packages`, in package order.
#[must_use]
pub fn violations(packages: &[Package], allow: &[String]) -> Vec<Violation> {
    packages
        .iter()
        .filter(|package| is_core(&package.name))
        .flat_map(|package| package_violations(package, allow))
        .collect()
}

fn package_violations(package: &Package, allow: &[String]) -> Vec<Violation> {
    let name = || package.name.clone();
    let mut found = Vec::new();
    match &package.clippy_config {
        None => found.push(Violation::MissingClippyConfig { package: name() }),
        Some(config) => {
            let banned = banned_paths(config);
            found.extend(
                REQUIRED_BANS
                    .iter()
                    .filter(|path| !banned.contains(path))
                    .map(|path| Violation::MissingBan {
                        package: name(),
                        path: (*path).to_owned(),
                    }),
            );
        }
    }
    match &package.lib_root {
        None => found.push(Violation::MissingLibRoot { package: name() }),
        Some(root) => {
            let forbidden = forbidden_lints(root);
            found.extend(
                REQUIRED_FORBID
                    .iter()
                    .filter(|lint| !forbidden.iter().any(|f| f == *lint))
                    .map(|lint| Violation::MissingForbid {
                        package: name(),
                        lint: (*lint).to_owned(),
                    }),
            );
        }
    }
    found.extend(
        package
            .dependencies
            .iter()
            .filter(|dep| dep.kind == DependencyKind::Normal)
            .filter(|dep| !is_core(&dep.name) && !allow.contains(&dep.name))
            .map(|dep| Violation::Dependency {
                package: name(),
                dependency: dep.name.clone(),
            }),
    );
    found
}

#[cfg(test)]
mod tests {
    use super::{
        Dependency, DependencyKind, Package, REQUIRED_BANS, REQUIRED_FORBID, Violation, violations,
    };
    use proptest::prelude::*;

    const CLIPPY: &str = include_str!("../../hypervisor-core/clippy.toml");
    const XTASK_CLIPPY: &str = include_str!("../clippy.toml");
    const CORE_ROOT: &str = include_str!("../../hypervisor-core/src/lib.rs");
    const XTASK_ROOT: &str = include_str!("lib.rs");

    fn dep(name: &str, kind: DependencyKind) -> Dependency {
        Dependency {
            name: name.to_owned(),
            kind,
        }
    }

    fn core(config: Option<&str>, root: Option<&str>, dependencies: Vec<Dependency>) -> Package {
        Package {
            name: "hypervisor-core".to_owned(),
            clippy_config: config.map(str::to_owned),
            lib_root: root.map(str::to_owned),
            dependencies,
        }
    }

    fn missing_forbid(lint: &str) -> Violation {
        Violation::MissingForbid {
            package: "hypervisor-core".to_owned(),
            lint: lint.to_owned(),
        }
    }

    #[test]
    fn committed_core_crates_pass() {
        let packages = [
            core(Some(CLIPPY), Some(CORE_ROOT), vec![]),
            core(Some(XTASK_CLIPPY), Some(XTASK_ROOT), vec![]),
        ];
        assert_eq!(violations(&packages, &[]), vec![]);
    }

    #[test]
    fn core_crate_with_outside_normal_dependency_is_a_violation() {
        let packages = [core(
            Some(CLIPPY),
            Some(CORE_ROOT),
            vec![dep("serde_json", DependencyKind::Normal)],
        )];
        assert_eq!(
            violations(&packages, &[]),
            vec![Violation::Dependency {
                package: "hypervisor-core".to_owned(),
                dependency: "serde_json".to_owned(),
            }]
        );
    }

    #[test]
    fn core_dependencies_allowlisted_and_dev_and_build_dependencies_pass() {
        let packages = [core(
            Some(CLIPPY),
            Some(CORE_ROOT),
            vec![
                dep("xtask-core", DependencyKind::Normal),
                dep("thiserror", DependencyKind::Normal),
                dep("proptest", DependencyKind::Development),
                dep("cc", DependencyKind::Build),
            ],
        )];
        assert_eq!(violations(&packages, &["thiserror".to_owned()]), vec![]);
    }

    #[test]
    fn shell_crates_are_not_checked() {
        let packages = [Package {
            name: "hypervisord".to_owned(),
            clippy_config: None,
            lib_root: None,
            dependencies: vec![dep("serde_json", DependencyKind::Normal)],
        }];
        assert_eq!(violations(&packages, &[]), vec![]);
    }

    #[test]
    fn missing_clippy_config_is_a_violation() {
        assert_eq!(
            violations(&[core(None, Some(CORE_ROOT), vec![])], &[]),
            vec![Violation::MissingClippyConfig {
                package: "hypervisor-core".to_owned(),
            }]
        );
    }

    #[test]
    fn emptied_clippy_config_reports_every_ban() {
        let found = violations(&[core(Some(""), Some(CORE_ROOT), vec![])], &[]);
        assert_eq!(found.len(), REQUIRED_BANS.len());
        assert!(
            found
                .iter()
                .all(|v| matches!(v, Violation::MissingBan { .. }))
        );
    }

    #[test]
    fn commented_out_ban_does_not_count() {
        let config = CLIPPY.replace(
            "  { path = \"std::fs::read_to_string\"",
            "# { path = \"std::fs::read_to_string\"",
        );
        assert_eq!(
            violations(&[core(Some(&config), Some(CORE_ROOT), vec![])], &[]),
            vec![Violation::MissingBan {
                package: "hypervisor-core".to_owned(),
                path: "std::fs::read_to_string".to_owned(),
            }]
        );
    }

    #[test]
    fn deleted_forbid_reports_every_lint() {
        let root = "//! Docs.\n\npub fn f() {}\n";
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(root), vec![])], &[]),
            REQUIRED_FORBID
                .iter()
                .map(|l| missing_forbid(l))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn forbid_of_unsafe_code_alone_is_not_enough() {
        let root = "#![forbid(unsafe_code)]\n";
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(root), vec![])], &[]),
            REQUIRED_FORBID[1..]
                .iter()
                .map(|l| missing_forbid(l))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn commented_out_forbid_does_not_count() {
        let root = format!("// #![forbid({})]\n", REQUIRED_FORBID.join(", "));
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]).len(),
            REQUIRED_FORBID.len()
        );
    }

    #[test]
    fn block_commented_forbid_does_not_override_allow() {
        let root = format!(
            "/* #![forbid({})] */\n#![allow(clippy::disallowed_methods)]\n",
            REQUIRED_FORBID.join(", ")
        );
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
            REQUIRED_FORBID
                .iter()
                .map(|lint| missing_forbid(lint))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn nested_block_commented_forbid_does_not_count() {
        let root = format!(
            "/* outer /* nested */ #![forbid({})] */\n",
            REQUIRED_FORBID.join(", ")
        );
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
            REQUIRED_FORBID
                .iter()
                .map(|lint| missing_forbid(lint))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn forbid_inside_string_and_char_literals_does_not_count() {
        let root = format!(
            "const TEXT: &str = \"#![forbid({})]\";\nconst QUOTE: char = '#';\n",
            REQUIRED_FORBID.join(", ")
        );
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
            REQUIRED_FORBID
                .iter()
                .map(|lint| missing_forbid(lint))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn forbid_inside_raw_string_with_embedded_quotes_and_hashes_does_not_count() {
        let root = format!(
            "const TEXT: &str = r###\"\"## #![forbid({})] \"#\"###;\n",
            REQUIRED_FORBID.join(", ")
        );
        assert_eq!(
            violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
            REQUIRED_FORBID
                .iter()
                .map(|lint| missing_forbid(lint))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn real_forbid_after_literals_still_counts() {
        let root = format!(
            "const TEXT: &str = \"// \\\" /*\";\nconst QUOTE: char = '\"';\nconst APOSTROPHE: char = '\\'';\nconst RAW: &str = r##\"\"# /*\"##;\n#![forbid({})]\n",
            REQUIRED_FORBID.join(", ")
        );
        assert!(violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]).is_empty());
    }

    #[test]
    fn missing_lib_root_is_a_violation() {
        assert_eq!(
            violations(&[core(Some(CLIPPY), None, vec![])], &[]),
            vec![Violation::MissingLibRoot {
                package: "hypervisor-core".to_owned(),
            }]
        );
    }

    fn kind() -> impl Strategy<Value = DependencyKind> {
        prop_oneof![
            Just(DependencyKind::Normal),
            Just(DependencyKind::Development),
            Just(DependencyKind::Build),
        ]
    }

    fn dependencies() -> impl Strategy<Value = Vec<Dependency>> {
        prop::collection::vec(
            ("[a-z_]{1,8}(-core)?", kind()).prop_map(|(name, kind)| Dependency { name, kind }),
            0..6,
        )
    }

    proptest! {
        #[test]
        fn non_core_packages_never_violate(name in "[a-z]{1,8}", deps in dependencies()) {
            let package = Package { name, clippy_config: None, lib_root: None, dependencies: deps };
            prop_assert!(violations(&[package], &[]).is_empty());
        }

        #[test]
        fn one_violation_per_outside_normal_dependency(deps in dependencies()) {
            let expected = deps
                .iter()
                .filter(|d| d.kind == DependencyKind::Normal && !d.name.ends_with("-core"))
                .count();
            prop_assert_eq!(violations(&[core(Some(CLIPPY), Some(CORE_ROOT), deps)], &[]).len(), expected);
        }

        #[test]
        fn allowlisting_every_dependency_clears_the_package(deps in dependencies()) {
            let allow: Vec<String> = deps.iter().map(|d| d.name.clone()).collect();
            prop_assert!(violations(&[core(Some(CLIPPY), Some(CORE_ROOT), deps)], &allow).is_empty());
        }

        #[test]
        fn dropping_any_ban_reports_exactly_that_ban(index in 0..REQUIRED_BANS.len()) {
            let path = REQUIRED_BANS[index];
            let config: String = CLIPPY
                .lines()
                .filter(|line| !line.contains(&format!("path = \"{path}\"")))
                .collect::<Vec<_>>()
                .join("\n");
            prop_assert_eq!(
                violations(&[core(Some(&config), Some(CORE_ROOT), vec![])], &[]),
                vec![Violation::MissingBan { package: "hypervisor-core".to_owned(), path: path.to_owned() }]
            );
        }

        #[test]
        fn dropping_any_forbidden_lint_reports_exactly_that_lint(index in 0..REQUIRED_FORBID.len(), split in any::<bool>()) {
            let kept: Vec<&str> = REQUIRED_FORBID.iter().copied().filter(|l| *l != REQUIRED_FORBID[index]).collect();
            let root = if split {
                kept.iter().map(|l| format!("#![forbid(\n    {l},\n)]\n")).collect::<Vec<_>>().concat()
            } else {
                format!("#![forbid({})]\n", kept.join(", "))
            };
            prop_assert_eq!(
                violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
                vec![missing_forbid(REQUIRED_FORBID[index])]
            );
        }

        #[test]
        fn nested_comment_never_supplies_forbid(depth in 1..6usize) {
            let root = format!(
                "{}#![forbid({})]{}",
                "/*".repeat(depth),
                REQUIRED_FORBID.join(", "),
                "*/".repeat(depth)
            );
            prop_assert_eq!(
                violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
                REQUIRED_FORBID.iter().map(|lint| missing_forbid(lint)).collect::<Vec<_>>()
            );
        }

        #[test]
        fn raw_string_delimiter_requires_every_hash(hashes in 1..6usize) {
            let delimiter = "#".repeat(hashes);
            let shorter = "#".repeat(hashes - 1);
            let root = format!(
                "const TEXT: &str = r{delimiter}\"\"{shorter} #![forbid({})] \"{delimiter};",
                REQUIRED_FORBID.join(", ")
            );
            prop_assert_eq!(
                violations(&[core(Some(CLIPPY), Some(&root), vec![])], &[]),
                REQUIRED_FORBID.iter().map(|lint| missing_forbid(lint)).collect::<Vec<_>>()
            );
        }
    }
}
