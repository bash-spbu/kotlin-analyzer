//! Repository chores: `cargo xtask check` runs everything a commit must pass.
//!
//! - the layering rule: each crate depends only on the internal crates its
//!   layer allows (see [`ALLOWED_DEPENDENCIES`]);
//! - `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Which internal crates each crate may depend on. Lower layers never see
/// higher ones; features (`ide`) never reach past `hir` into `hir-def` or
/// `hir-ty`. Only the binary sees everything.
const ALLOWED_DEPENDENCIES: &[(&str, &[&str])] = &[
    ("intern", &[]),
    ("vfs", &[]),
    ("syntax", &[]),
    ("project-model", &["intern"]),
    ("classfile", &["intern"]),
    ("base-db", &["intern", "vfs", "syntax", "project-model"]),
    (
        "hir-def",
        &[
            "intern",
            "base-db",
            "syntax",
            "vfs",
            "project-model",
            "classfile",
        ],
    ),
    ("hir-ty", &["intern", "hir-def", "base-db", "syntax", "vfs"]),
    (
        "hir",
        &[
            "intern",
            "hir-ty",
            "hir-def",
            "base-db",
            "syntax",
            "vfs",
            "project-model",
        ],
    ),
    (
        "ide",
        &["intern", "hir", "base-db", "syntax", "vfs", "project-model"],
    ),
    (
        "kotlin-analyzer",
        &[
            "intern",
            "vfs",
            "syntax",
            "project-model",
            "base-db",
            "hir-def",
            "hir-ty",
            "hir",
            "ide",
            "classfile",
        ],
    ),
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("check") => run_check(),
        Some("layers") => report(check_layers(&workspace_root())),
        _ => {
            eprintln!("usage: cargo xtask <check | layers>");
            ExitCode::FAILURE
        }
    }
}

fn run_check() -> ExitCode {
    let root = workspace_root();
    if let ExitCode::FAILURE = report(check_layers(&root)) {
        return ExitCode::FAILURE;
    }
    let steps: [&[&str]; 3] = [
        &["fmt", "--all", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--quiet"],
    ];
    for arguments in steps {
        let status = Command::new(env_cargo())
            .args(arguments)
            .current_dir(&root)
            .status();
        let passed = status.is_ok_and(|status| status.success());
        if !passed {
            eprintln!("xtask: `cargo {}` failed", arguments.join(" "));
            return ExitCode::FAILURE;
        }
    }
    eprintln!("xtask: all checks passed");
    ExitCode::SUCCESS
}

fn report(violations: Vec<String>) -> ExitCode {
    if violations.is_empty() {
        eprintln!("xtask: crate layers are respected");
        return ExitCode::SUCCESS;
    }
    for violation in &violations {
        eprintln!("xtask: {violation}");
    }
    ExitCode::FAILURE
}

/// Every internal dependency a crate declares that its layer doesn't allow,
/// and every crate missing from the table.
fn check_layers(root: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    let internal: Vec<&str> = ALLOWED_DEPENDENCIES.iter().map(|(name, _)| *name).collect();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return vec!["cannot read crates/".to_owned()];
    };
    for entry in entries.flatten() {
        let crate_name = entry.file_name().to_string_lossy().into_owned();
        let Some((_, allowed)) = ALLOWED_DEPENDENCIES
            .iter()
            .find(|(name, _)| *name == crate_name)
        else {
            violations.push(format!(
                "crate `{crate_name}` has no layer in ALLOWED_DEPENDENCIES"
            ));
            continue;
        };
        let manifest = std::fs::read_to_string(entry.path().join("Cargo.toml")).unwrap_or_default();
        for dependency in declared_dependencies(&manifest) {
            let is_internal = internal.contains(&dependency.as_str());
            if is_internal && !allowed.contains(&dependency.as_str()) {
                violations.push(format!(
                    "`{crate_name}` depends on `{dependency}`, which its layer doesn't allow"
                ));
            }
        }
    }
    violations
}

/// The names under `[dependencies]` and `[dev-dependencies]` of a manifest.
fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependencies = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_dependencies = matches!(line, "[dependencies]" | "[dev-dependencies]");
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line
            .split(['=', '.'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn env_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_dependencies_reads_both_sections_and_skips_others() {
        let manifest = "\
[package]
name = \"hir\"

[dependencies]
hir-ty.workspace = true
serde = { version = \"1\" }
# a comment

[dev-dependencies]
expect-test = \"1\"

[lints]
workspace = true
";
        assert_eq!(
            declared_dependencies(manifest),
            vec!["hir-ty", "serde", "expect-test"]
        );
    }

    #[test]
    fn the_workspace_respects_its_layers() {
        assert_eq!(check_layers(&workspace_root()), Vec::<String>::new());
    }
}
