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
    UnnecessaryNotNullAssertion,
    UnnecessarySafeCall,
    UselessElvis,
    SenselessComparison,
    UnusedVariable,
    CanBeVal,
    Deprecation,
    DeprecationError,
    OptInUsage,
    OptInUsageError,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
    /// A warning Kotlin's tools give too — the compiler, or IntelliJ's
    /// extended checks: code that compiles may have it, so the corpus gate
    /// can't call it false.
    Lint,
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
    pub const ALL: [Check; 18] = [
        Check::Syntax,
        Check::UnresolvedReference,
        Check::MissingImport,
        Check::ArgumentMismatch,
        Check::UnsafeCall,
        Check::NonExhaustiveWhen,
        Check::UnusedImport,
        Check::MissingPackage,
        Check::UnnecessaryNotNullAssertion,
        Check::UnnecessarySafeCall,
        Check::UselessElvis,
        Check::SenselessComparison,
        Check::UnusedVariable,
        Check::CanBeVal,
        Check::Deprecation,
        Check::DeprecationError,
        Check::OptInUsage,
        Check::OptInUsageError,
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
            Check::UnnecessaryNotNullAssertion => "unnecessary-not-null-assertion",
            Check::UnnecessarySafeCall => "unnecessary-safe-call",
            Check::UselessElvis => "useless-elvis",
            Check::SenselessComparison => "senseless-comparison",
            Check::UnusedVariable => "unused-variable",
            Check::CanBeVal => "can-be-val",
            Check::Deprecation => "deprecation",
            Check::DeprecationError => "deprecation-error",
            Check::OptInUsage => "opt-in-usage",
            Check::OptInUsageError => "opt-in-usage-error",
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
            | Check::NonExhaustiveWhen
            | Check::DeprecationError
            | Check::OptInUsageError => Severity::Error,
            Check::MissingImport | Check::MissingPackage => Severity::Warning,
            Check::UnnecessaryNotNullAssertion
            | Check::UnnecessarySafeCall
            | Check::UselessElvis
            | Check::SenselessComparison
            | Check::CanBeVal
            | Check::Deprecation
            | Check::OptInUsage => Severity::Lint,
            Check::UnusedImport | Check::UnusedVariable => Severity::Unnecessary,
        }
    }

    /// The names that turn the check off in `@Suppress(…)`: the ones the
    /// Kotlin compiler or IntelliJ give the same finding.
    pub fn suppressed_by(self) -> &'static [&'static str] {
        match self {
            Check::MissingPackage => &["PackageDirectoryMismatch"],
            Check::UnusedImport => &["UnusedImport"],
            Check::UnnecessaryNotNullAssertion => &["UNNECESSARY_NOT_NULL_ASSERTION"],
            Check::UnnecessarySafeCall => &["UNNECESSARY_SAFE_CALL"],
            Check::UselessElvis => &["USELESS_ELVIS"],
            Check::SenselessComparison => &["SENSELESS_COMPARISON"],
            Check::UnusedVariable => &["UNUSED_VARIABLE", "unused"],
            Check::CanBeVal => &["CAN_BE_VAL", "CanBeVal"],
            Check::Deprecation => &["DEPRECATION", "TYPEALIAS_EXPANSION_DEPRECATION"],
            Check::DeprecationError => {
                &["DEPRECATION_ERROR", "TYPEALIAS_EXPANSION_DEPRECATION_ERROR"]
            }
            Check::OptInUsage => &["OPT_IN_USAGE"],
            Check::OptInUsageError => &["OPT_IN_USAGE_ERROR"],
            Check::Syntax
            | Check::UnresolvedReference
            | Check::MissingImport
            | Check::ArgumentMismatch
            | Check::UnsafeCall
            | Check::NonExhaustiveWhen => &[],
        }
    }

    /// The compiler's own name for the warning, when the compiler reports
    /// it — not an IDE-only check: its level follows the source set's
    /// compiler options (`-Werror`, `-nowarn`, `-Xwarning-level`).
    pub fn compiler_warning(self) -> Option<&'static str> {
        match self {
            Check::UnnecessaryNotNullAssertion => Some("UNNECESSARY_NOT_NULL_ASSERTION"),
            Check::UnnecessarySafeCall => Some("UNNECESSARY_SAFE_CALL"),
            Check::UselessElvis => Some("USELESS_ELVIS"),
            Check::SenselessComparison => Some("SENSELESS_COMPARISON"),
            Check::Deprecation => Some("DEPRECATION"),
            Check::OptInUsage => Some("OPT_IN_USAGE"),
            _ => None,
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
    /// The check's, unless the compiler options change it (a warning the
    /// build makes an error).
    pub severity: Severity,
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
            severity: Check::Syntax.severity(),
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
    let suppressions = suppression::suppressions(&parse);
    found.retain(|diagnostic| !suppressions.suppress(diagnostic));
    let options = hir::compiler_options(db, source);
    found.retain_mut(|diagnostic| {
        let Some(name) = diagnostic.check.compiler_warning() else {
            return true;
        };
        match options.warning_level(name) {
            hir::WarningLevel::Warning => true,
            hir::WarningLevel::Error => {
                diagnostic.severity = Severity::Error;
                true
            }
            hir::WarningLevel::Disabled => false,
        }
    });
    found.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.check));
    found
}
