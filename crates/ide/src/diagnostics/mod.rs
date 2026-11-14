//! Diagnostics: each check renders facts the semantic model computed and
//! stays silent whenever something it would need is unknown (see
//! `docs/spec/diagnostics.md`).

mod facts;
pub(crate) mod imports;
mod package;
mod suppression;

use hir::{FileId, RootDatabase, SourceDatabase};
use syntax::{Language, TextRange};

/// A check, by the name settings and the CLI use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Check {
    Syntax,
    UnresolvedReference,
    MissingImport,
    ArgumentMismatch,
    UnsafeCall,
    NonExhaustiveWhen,
    UnusedImport,
    MissingPackage,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
    /// Shown faded, as code that can go (`unused-import`).
    Unnecessary,
}

/// Whether a check runs by default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stability {
    /// Passed the corpus gate: on unless disabled.
    Stable,
    /// Off unless experimental checks are enabled.
    Experimental,
}

impl Check {
    pub const ALL: [Check; 8] = [
        Check::Syntax,
        Check::UnresolvedReference,
        Check::MissingImport,
        Check::ArgumentMismatch,
        Check::UnsafeCall,
        Check::NonExhaustiveWhen,
        Check::UnusedImport,
        Check::MissingPackage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Check::Syntax => "syntax",
            Check::UnresolvedReference => "unresolved-reference",
            Check::MissingImport => "missing-import",
            Check::ArgumentMismatch => "argument-mismatch",
            Check::UnsafeCall => "unsafe-call",
            Check::NonExhaustiveWhen => "non-exhaustive-when",
            Check::UnusedImport => "unused-import",
            Check::MissingPackage => "missing-package",
        }
    }

    pub fn from_name(name: &str) -> Option<Check> {
        Check::ALL.into_iter().find(|check| check.name() == name)
    }

    pub fn severity(self) -> Severity {
        match self {
            Check::Syntax
            | Check::UnresolvedReference
            | Check::ArgumentMismatch
            | Check::UnsafeCall
            | Check::NonExhaustiveWhen => Severity::Error,
            Check::MissingImport | Check::MissingPackage => Severity::Warning,
            Check::UnusedImport => Severity::Unnecessary,
        }
    }

    /// The names that turn the check off in `@file:Suppress(…)`: the ones
    /// the Kotlin tools give the same finding.
    pub fn suppressed_by(self) -> &'static [&'static str] {
        match self {
            Check::MissingPackage => &["PackageDirectoryMismatch"],
            Check::UnusedImport => &["UnusedImport"],
            Check::Syntax
            | Check::UnresolvedReference
            | Check::MissingImport
            | Check::ArgumentMismatch
            | Check::UnsafeCall
            | Check::NonExhaustiveWhen => &[],
        }
    }

    pub fn stability(self) -> Stability {
        match self {
            Check::UnresolvedReference => Stability::Experimental,
            _ => Stability::Stable,
        }
    }
}

/// Which checks run.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticsConfig {
    pub disabled: Vec<Check>,
    /// Run experimental checks too.
    pub experimental: bool,
    /// Run only these (when not empty).
    pub only: Vec<Check>,
}

impl DiagnosticsConfig {
    pub fn runs(&self, check: Check) -> bool {
        if !self.only.is_empty() {
            return self.only.contains(&check);
        }
        let enabled = check.stability() == Stability::Stable || self.experimental;
        enabled && !self.disabled.contains(&check)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub check: Check,
    pub range: TextRange,
    pub message: String,
    pub fixes: Vec<Fix>,
}

/// An edit that resolves a diagnostic.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fix {
    pub label: String,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextEdit {
    pub range: TextRange,
    pub new_text: String,
}

pub(crate) fn diagnostics(
    db: &RootDatabase,
    file: FileId,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let parse = hir::parse(db, source);
    let mut found = Vec::new();
    if config.runs(Check::Syntax) {
        found.extend(parse.errors().into_iter().map(|error| Diagnostic {
            check: Check::Syntax,
            range: error.range,
            message: error.message,
            fixes: Vec::new(),
        }));
    }
    // A file that doesn't parse cleanly would make every other check guess.
    let semantic = source.language(db) == Language::Kotlin && parse.errors().is_empty();
    if semantic {
        found.extend(imports::diagnostics(db, source, &parse, config));
        found.extend(facts::diagnostics(db, source, config));
        if config.runs(Check::MissingPackage) {
            found.extend(package::missing_package(db, source, &parse));
        }
    }
    let suppressed = suppression::file_suppressions(&parse);
    found.retain(|diagnostic| {
        !diagnostic
            .check
            .suppressed_by()
            .iter()
            .any(|name| suppressed.contains(*name))
    });
    found.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.check));
    found
}
