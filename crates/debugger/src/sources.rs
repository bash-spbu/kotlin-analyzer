//! Source files and the classes compiled from them. A class records the
//! name of its source file (`Order.kt`) and its package is its name's
//! prefix; a source file says its package. So a breakpoint's file gives
//! the classes to look in, and a frame's class gives the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The package a Kotlin or Java file declares (`package a.b`), `""` for
/// none.
pub fn declared_package(text: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("package ") else {
            let is_preamble = line.is_empty()
                || line.starts_with("//")
                || line.starts_with("/*")
                || line.starts_with('*')
                || line.starts_with('@');
            if is_preamble {
                continue;
            }
            break;
        };
        return rest
            .trim_end_matches(';')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .replace('`', "");
    }
    String::new()
}

/// What a breakpoint's file is to classes: its file name and package.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceKey {
    /// `Order.kt`.
    pub file_name: String,
    /// `com.example`.
    pub package: String,
    /// It declares inline functions: other files' classes may hold its
    /// code.
    pub declares_inline_functions: bool,
}

impl SourceKey {
    pub fn of(path: &Path) -> std::io::Result<SourceKey> {
        let text = std::fs::read_to_string(path)?;
        Ok(SourceKey {
            file_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            package: declared_package(&text),
            declares_inline_functions: text.contains("inline fun "),
        })
    }

    /// Whether a class with this JNI signature (`Lcom/example/Order$1;`)
    /// is in the key's package — not a subpackage's.
    pub fn holds_class(&self, signature: &str) -> bool {
        let Some(name) = class_name(signature) else {
            return false;
        };
        let package = name.rsplit_once('.').map_or("", |(package, _)| package);
        package == self.package
    }

    /// The pattern of the classes of the key's package, for a class
    /// prepare request (`com.example.*`).
    pub fn class_pattern(&self) -> String {
        if self.package.is_empty() {
            "*".to_owned()
        } else {
            format!("{}.*", self.package)
        }
    }
}

/// `com.example.Order$Line` of `Lcom/example/Order$Line;`.
pub fn class_name(signature: &str) -> Option<String> {
    let inner = signature.strip_prefix('L')?.strip_suffix(';')?;
    Some(inner.replace('/', "."))
}

/// The project's source files, by file name: what a frame's class maps
/// back to.
pub struct SourceIndex {
    by_name: HashMap<String, Vec<PathBuf>>,
    packages: HashMap<PathBuf, String>,
}

impl SourceIndex {
    /// Every `.kt`, `.kts` and `.java` under `roots`, build outputs and
    /// hidden directories left out.
    pub fn of(roots: &[PathBuf]) -> SourceIndex {
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for root in roots {
            let walk = walkdir::WalkDir::new(root)
                .into_iter()
                .filter_entry(|entry| {
                    let name = entry.file_name().to_string_lossy();
                    let skipped = entry.depth() > 0
                        && entry.file_type().is_dir()
                        && (name.starts_with('.')
                            || matches!(
                                name.as_ref(),
                                "build" | "out" | "target" | "node_modules"
                            ));
                    !skipped
                });
            for entry in walk.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_source =
                    name.ends_with(".kt") || name.ends_with(".kts") || name.ends_with(".java");
                if entry.file_type().is_file() && is_source {
                    by_name.entry(name).or_default().push(entry.into_path());
                }
            }
        }
        SourceIndex {
            by_name,
            packages: HashMap::new(),
        }
    }

    /// The file `file_name` of `package`: the one declaring that package
    /// when several have the name.
    pub fn find(&mut self, file_name: &str, package: &str) -> Option<PathBuf> {
        let candidates = self.by_name.get(file_name)?.clone();
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
        candidates.into_iter().find(|path| {
            let declared = self.packages.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(path)
                    .map(|text| declared_package(&text))
                    .unwrap_or_default()
            });
            declared == package
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_package_is_its_package_line_after_comments_and_annotations() {
        assert_eq!(
            declared_package(
                "/* licence */\n@file:JvmName(\"X\")\npackage com.example.`in`\n\nclass A"
            ),
            "com.example.in"
        );
        assert_eq!(declared_package("package demo;\nclass Main {}"), "demo");
        assert_eq!(declared_package("class Loose"), "");
    }

    #[test]
    fn a_key_holds_its_packages_classes_not_a_subpackages() {
        let key = SourceKey {
            file_name: "Order.kt".to_owned(),
            package: "com.example".to_owned(),
            declares_inline_functions: false,
        };
        assert!(key.holds_class("Lcom/example/OrderKt;"));
        assert!(key.holds_class("Lcom/example/Order$total$1;"));
        assert!(!key.holds_class("Lcom/example/sub/Order;"));
        assert_eq!(key.class_pattern(), "com.example.*");
        assert_eq!(
            class_name("Lcom/example/Order;").as_deref(),
            Some("com.example.Order")
        );
    }
}
