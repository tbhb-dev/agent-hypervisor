//! The functional core boundary.
//!
//! A crate whose name ends in `-core` is pure. Its normal dependencies must be other `-core`
//! crates or crates on the workspace allowlist, and it must carry a `clippy.toml` holding the
//! I/O ban. Development and build dependencies are not checked: they never reach the library.

use std::fmt;

/// The suffix that marks a pure crate.
pub const CORE_SUFFIX: &str = "-core";

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
    pub has_clippy_config: bool,
    pub dependencies: Vec<Dependency>,
}

/// A breach of the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A core crate has a normal dependency that is neither a core crate nor allowlisted.
    Dependency { package: String, dependency: String },
    /// A core crate has no `clippy.toml` beside its manifest, so the I/O ban does not apply.
    MissingClippyConfig { package: String },
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
        }
    }
}

/// Reports whether a crate name marks a pure crate.
#[must_use]
pub fn is_core(name: &str) -> bool {
    name.ends_with(CORE_SUFFIX)
}

/// Returns every boundary violation among `packages`, in package and dependency order.
#[must_use]
pub fn violations(packages: &[Package], allow: &[String]) -> Vec<Violation> {
    packages
        .iter()
        .filter(|package| is_core(&package.name))
        .flat_map(|package| {
            let missing = (!package.has_clippy_config).then(|| Violation::MissingClippyConfig {
                package: package.name.clone(),
            });
            let deps = package
                .dependencies
                .iter()
                .filter(|dep| dep.kind == DependencyKind::Normal)
                .filter(|dep| !is_core(&dep.name) && !allow.contains(&dep.name))
                .map(|dep| Violation::Dependency {
                    package: package.name.clone(),
                    dependency: dep.name.clone(),
                });
            missing.into_iter().chain(deps)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Dependency, DependencyKind, Package, Violation, violations};
    use proptest::prelude::*;

    fn dep(name: &str, kind: DependencyKind) -> Dependency {
        Dependency {
            name: name.to_owned(),
            kind,
        }
    }

    fn package(name: &str, has_clippy_config: bool, dependencies: Vec<Dependency>) -> Package {
        Package {
            name: name.to_owned(),
            has_clippy_config,
            dependencies,
        }
    }

    #[test]
    fn core_crate_with_outside_normal_dependency_is_a_violation() {
        let packages = [package(
            "hypervisor-core",
            true,
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
        let packages = [package(
            "hypervisor-core",
            true,
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
        let packages = [package(
            "hypervisord",
            false,
            vec![dep("serde_json", DependencyKind::Normal)],
        )];
        assert_eq!(violations(&packages, &[]), vec![]);
    }

    #[test]
    fn core_crate_without_clippy_config_is_a_violation() {
        let packages = [package("hypervisor-core", false, vec![])];
        assert_eq!(
            violations(&packages, &[]),
            vec![Violation::MissingClippyConfig {
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
        fn non_core_packages_never_violate(name in "[a-z]{1,8}", deps in dependencies(), clippy in any::<bool>()) {
            prop_assert!(violations(&[package(&name, clippy, deps)], &[]).is_empty());
        }

        #[test]
        fn one_violation_per_outside_normal_dependency(deps in dependencies()) {
            let expected = deps
                .iter()
                .filter(|d| d.kind == DependencyKind::Normal && !d.name.ends_with("-core"))
                .count();
            prop_assert_eq!(violations(&[package("a-core", true, deps)], &[]).len(), expected);
        }

        #[test]
        fn allowlisting_every_dependency_clears_the_package(deps in dependencies()) {
            let allow: Vec<String> = deps.iter().map(|d| d.name.clone()).collect();
            prop_assert!(violations(&[package("a-core", true, deps)], &allow).is_empty());
        }
    }
}
