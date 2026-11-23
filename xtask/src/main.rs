//! Repository chores: `cargo xtask check` runs everything a commit must pass.
//!
//! - the layering rule: each crate depends only on the internal crates its
//!   layer allows (see [`ALLOWED_DEPENDENCIES`]);
//! - the editor integrations download the server's own version (see
//!   [`check_versions`]); `cargo xtask versions` runs only this;
//! - the Zed and VS Code extensions pin the same formatter releases (see
//!   [`check_formatter_pins`]);
//! - `unsafe` only where [`UNSAFE_ALLOWED`] says (see [`check_unsafe`]);
//! - `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and
//!   clippy and the profiling tests of the `memory-profile` build;
//! - the editors' Gradle tasks: `editors/gradle/test_gradle_task.py`.

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
    ("jdwp", &[]),
    ("debugger", &["jdwp", "project-model"]),
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
            "jdwp",
            "debugger",
        ],
    ),
];

/// The only files that may allow `unsafe_code` (denied in the workspace):
/// the profiling build's allocator.
const UNSAFE_ALLOWED: &[&str] = &["crates/kotlin-analyzer/src/memory_profile/allocator.rs"];

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("check") => run_check(),
        Some("layers") => report(check_layers(&workspace_root())),
        Some("versions") => report(check_versions(&workspace_root())),
        _ => {
            eprintln!("usage: cargo xtask <check | layers | versions>");
            ExitCode::FAILURE
        }
    }
}

fn run_check() -> ExitCode {
    let root = workspace_root();
    if let ExitCode::FAILURE = report(check_layers(&root)) {
        return ExitCode::FAILURE;
    }
    if let ExitCode::FAILURE = report(check_versions(&root)) {
        return ExitCode::FAILURE;
    }
    if let ExitCode::FAILURE = report(check_formatter_pins(&root)) {
        return ExitCode::FAILURE;
    }
    if let ExitCode::FAILURE = report(check_unsafe(&root)) {
        return ExitCode::FAILURE;
    }
    let steps: [&[&str]; 6] = [
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
        // The profiling build: its allocator and query scopes.
        &[
            "clippy",
            "--package",
            "kotlin-analyzer",
            "--all-targets",
            "--features",
            "memory-profile",
            "--",
            "-D",
            "warnings",
        ],
        &[
            "test",
            "--package",
            "kotlin-analyzer",
            "--features",
            "memory-profile",
            "--quiet",
            "memory_",
        ],
        // The Zed extension is a crate of its own (built for wasm); its
        // tests run on the host.
        &[
            "test",
            "--manifest-path",
            "editors/zed/Cargo.toml",
            "--quiet",
        ],
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
    let gradle_tasks = Command::new("python3")
        .args(["-I", "editors/gradle/test_gradle_task.py"])
        .current_dir(&root)
        .status();
    if !gradle_tasks.is_ok_and(|status| status.success()) {
        eprintln!("xtask: editors/gradle/test_gradle_task.py failed");
        return ExitCode::FAILURE;
    }
    eprintln!("xtask: all checks passed");
    ExitCode::SUCCESS
}

fn report(violations: Vec<String>) -> ExitCode {
    if violations.is_empty() {
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

/// Every Rust file under `crates/` that allows `unsafe_code` but isn't in
/// [`UNSAFE_ALLOWED`].
fn check_unsafe(root: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    let mut directories = vec![root.join("crates")];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            let is_rust = path.extension().is_some_and(|extension| extension == "rs");
            if !is_rust {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if !text.contains("allow(unsafe_code)") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if !UNSAFE_ALLOWED.contains(&relative.as_str()) {
                violations.push(format!(
                    "{relative} allows `unsafe_code`; only {UNSAFE_ALLOWED:?} may"
                ));
            }
        }
    }
    violations
}

/// Where an editor integration states its version: the file, and the text
/// the version follows (in quotes).
const EDITOR_VERSIONS: &[(&str, &str)] = &[
    ("editors/vscode/package.json", "\"version\":"),
    ("editors/neovim/lua/kotlin_analyzer/version.lua", "return"),
    ("editors/zed/src/release.rs", "SERVER_VERSION: &str ="),
    (
        "editors/claude-code/.claude-plugin/plugin.json",
        "\"version\":",
    ),
];

/// Every editor integration whose version isn't the server's. The
/// extensions download the release tagged with their own version, and the
/// Claude Code plugin is released with the server, so they must agree.
fn check_versions(root: &Path) -> Vec<String> {
    let workspace_manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let Some(server_version) = workspace_version(&workspace_manifest) else {
        return vec!["Cargo.toml has no [workspace.package] version".to_owned()];
    };
    let mut violations = Vec::new();
    for (file, marker) in EDITOR_VERSIONS {
        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        match quoted_after(&text, marker) {
            Some(version) if version == server_version => {}
            Some(version) => violations.push(format!(
                "{file} is at version {version}, the server at {server_version}"
            )),
            None => violations.push(format!("{file} states no version after `{marker}`")),
        }
    }
    violations
}

/// Where each editor extension pins the formatters it downloads: the file,
/// and the text each version follows.
const FORMATTER_PINS: [(&str, [&str; 2]); 2] = [
    (
        "editors/zed/src/formatter.rs",
        ["KTRS_VERSION: &str =", "KTFMT_VERSION: &str ="],
    ),
    (
        "editors/vscode/src/formatter.ts",
        ["KTRS_VERSION =", "KTFMT_VERSION ="],
    ),
];

/// What one extension pins: the formatters' versions and checksums.
#[derive(PartialEq, Eq, Debug)]
struct FormatterPins {
    versions: Vec<Option<String>>,
    digests: Vec<String>,
}

impl FormatterPins {
    fn read(root: &Path, file: &str, markers: &[&str]) -> FormatterPins {
        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        FormatterPins {
            versions: markers
                .iter()
                .map(|marker| quoted_after(&text, marker).map(str::to_owned))
                .collect(),
            digests: sha256_digests(&text),
        }
    }
}

/// Where the Zed and VS Code extensions pin different formatter versions
/// or checksums: both download what they pin, so a user gets the same
/// formatter in either.
fn check_formatter_pins(root: &Path) -> Vec<String> {
    let [(zed, zed_markers), (vscode, vscode_markers)] = FORMATTER_PINS;
    let zed_pins = FormatterPins::read(root, zed, &zed_markers);
    let vscode_pins = FormatterPins::read(root, vscode, &vscode_markers);
    let mut violations = Vec::new();
    let any_unstated = zed_pins.versions.contains(&None) || vscode_pins.versions.contains(&None);
    if any_unstated {
        violations.push(format!("{zed} or {vscode} states no formatter version"));
    }
    if zed_pins.versions != vscode_pins.versions {
        violations.push(format!(
            "{zed} pins the formatters at {:?}, {vscode} at {:?}",
            zed_pins.versions, vscode_pins.versions
        ));
    }
    if zed_pins.digests.is_empty() || zed_pins.digests != vscode_pins.digests {
        violations.push(format!(
            "{zed} and {vscode} pin different formatter checksums"
        ));
    }
    violations
}

/// The SHA-256 digests (64 lowercase hex digits in quotes) in `text`,
/// sorted.
fn sha256_digests(text: &str) -> Vec<String> {
    let mut digests: Vec<String> = text
        .split('"')
        .filter(|quoted| {
            quoted.len() == 64
                && quoted
                    .chars()
                    .all(|char| char.is_ascii_digit() || ('a'..='f').contains(&char))
        })
        .map(str::to_owned)
        .collect();
    digests.sort();
    digests.dedup();
    digests
}

/// The `version` of a manifest's `[workspace.package]`.
fn workspace_version(manifest: &str) -> Option<&str> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[workspace.package]";
            continue;
        }
        if in_package && line.starts_with("version") {
            return quoted_after(line, "version");
        }
    }
    None
}

/// The first quoted string after the first `marker` in `text`.
fn quoted_after<'text>(text: &'text str, marker: &str) -> Option<&'text str> {
    let after_marker = &text[text.find(marker)? + marker.len()..];
    let opening = after_marker.find('"')?;
    let quoted = &after_marker[opening + 1..];
    let closing = quoted.find('"')?;
    Some(&quoted[..closing])
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
    fn workspace_version_reads_only_the_workspace_package() {
        let manifest = "\
[package]
version = \"9.9.9\"

[workspace.package]
edition = \"2024\"
version = \"0.3.1\"
";
        assert_eq!(workspace_version(manifest), Some("0.3.1"));
        assert_eq!(workspace_version("[package]\nversion = \"1.0.0\"\n"), None);
    }

    #[test]
    fn quoted_after_takes_the_first_string_after_the_marker() {
        let package_json = "{ \"name\": \"x\", \"version\": \"1.2.3\", \"engines\": {} }";
        assert_eq!(quoted_after(package_json, "\"version\":"), Some("1.2.3"));
        assert_eq!(quoted_after("return \"0.1.0\"\n", "return"), Some("0.1.0"));
        assert_eq!(quoted_after("return nil", "return"), None);
    }

    #[test]
    fn the_editors_download_the_servers_version() {
        assert_eq!(check_versions(&workspace_root()), Vec::<String>::new());
    }

    #[test]
    fn the_editors_pin_the_same_formatters() {
        assert_eq!(
            check_formatter_pins(&workspace_root()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn sha256_digests_are_quoted_lowercase_hex_of_64_digits() {
        let digest = "ab".repeat(32);
        let text = format!(
            "\"{digest}\" \"{}\" \"{}\" '{digest}' \"{digest}\"",
            "AB".repeat(32),
            "ab".repeat(31)
        );
        assert_eq!(sha256_digests(&text), vec![digest]);
    }

    #[test]
    fn unsafe_is_allowed_only_in_the_profiling_allocator() {
        assert_eq!(check_unsafe(&workspace_root()), Vec::<String>::new());
        let scratch = std::env::temp_dir().join(format!("xtask-unsafe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(scratch.join("crates/x/src")).unwrap();
        std::fs::write(
            scratch.join("crates/x/src/lib.rs"),
            "#![allow(unsafe_code)]\n",
        )
        .unwrap();
        std::fs::write(scratch.join("crates/x/src/safe.rs"), "fn f() {}\n").unwrap();
        assert_eq!(
            check_unsafe(&scratch),
            vec![format!(
                "crates/x/src/lib.rs allows `unsafe_code`; only {UNSAFE_ALLOWED:?} may"
            )]
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn the_workspace_respects_its_layers() {
        assert_eq!(check_layers(&workspace_root()), Vec::<String>::new());
    }
}
