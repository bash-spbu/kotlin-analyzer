//! Checks that render facts inference recorded: calls no candidate takes,
//! unsafe calls on nullable receivers, non-exhaustive `when`s, null checks
//! on values that can't be null, unused locals and `var`s that could be
//! `val`s.

use hir::{
    DeprecationLevel, KotlinVersion, LocalFinding, MismatchSite, NonExhaustiveWhen, NullCheckKind,
    OptInLevel, RootDatabase, SourceFile, TypeOperationKind, WhenFinding,
};
use syntax::{Parse, TextRange, has_token};

use super::{Check, Diagnostic, DiagnosticsConfig, Fix, TextEdit};

pub(super) fn diagnostics(
    db: &RootDatabase,
    file: SourceFile,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    let inference = hir::infer_file(db, file);
    let mut found = Vec::new();
    if config.runs(Check::ArgumentMismatch) {
        for mismatch in inference.argument_mismatches() {
            let name = file.text(db)[mismatch.range.start as usize..mismatch.range.end as usize]
                .to_owned();
            let message = match mismatch.candidates.len() {
                1 => format!("No `{name}` takes these arguments"),
                count => format!("None of the {count} `{name}` overloads takes these arguments"),
            };
            found.push(Diagnostic {
                check: Check::ArgumentMismatch,
                severity: Check::ArgumentMismatch.severity(),
                range: mismatch.range,
                message,
                fixes: Vec::new(),
            });
        }
    }
    if config.runs(Check::UnsafeCall) {
        for call in inference.unsafe_calls() {
            let receiver = hir::display_type(db, &call.receiver);
            found.push(Diagnostic {
                check: Check::UnsafeCall,
                severity: Check::UnsafeCall.severity(),
                range: call.range,
                message: format!("Only safe (`?.`) or non-null asserted (`!!.`) calls are allowed on a receiver of type `{receiver}`"),
                fixes: Vec::new(),
            });
        }
    }
    if config.runs(Check::NonExhaustiveWhen) {
        for when in inference.non_exhaustive_whens() {
            found.push(Diagnostic {
                check: Check::NonExhaustiveWhen,
                severity: Check::NonExhaustiveWhen.severity(),
                range: when.range,
                message: format!(
                    "`when` must be exhaustive: add the missing {} or an `else` branch",
                    when.missing
                        .iter()
                        .map(|case| format!("`{case}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                fixes: fill_when(&hir::parse(db, file), when).into_iter().collect(),
            });
        }
    }
    found.extend(null_checks(db, file, config));
    found.extend(local_findings(db, file, config));
    found.extend(deprecations(db, file, config));
    found.extend(opt_ins(db, file, config));
    for mismatch in inference.type_mismatches() {
        let (check, what) = match mismatch.site {
            MismatchSite::Return => (Check::ReturnTypeMismatch, "Return"),
            MismatchSite::Initializer => (Check::TypeMismatch, "Initializer"),
            MismatchSite::Assignment => (Check::TypeMismatch, "Assignment"),
            MismatchSite::Argument => (Check::TypeMismatch, "Argument"),
        };
        if config.runs(check) {
            let expected = hir::display_type(db, &mismatch.expected);
            let actual = hir::display_type(db, &mismatch.actual);
            found.push(Diagnostic {
                check,
                severity: check.severity(),
                range: mismatch.range,
                message: format!("{what} type mismatch: expected `{expected}`, actual `{actual}`"),
                fixes: Vec::new(),
            });
        }
    }
    found.extend(type_operations(db, file, config));
    found.extend(when_findings(db, file, config));
    found
}

/// The language version from which the compiler reports useless casts,
/// casts that never succeed, `!!`, `?.` and `?:` on a value that can't be
/// null (its data-flow warnings, `EnableDfaWarningsInK2`, `KT-50965`).
const DATA_FLOW_WARNINGS: KotlinVersion = KotlinVersion::new(2, 3);

/// Casts and type checks whose outcome the types decide, as Kotlin words
/// them.
fn type_operations(
    db: &RootDatabase,
    file: SourceFile,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    let inference = hir::infer_file(db, file);
    let data_flow_warnings = hir::compiler_options(db, file)
        .language_version()
        .is_some_and(|version| version >= DATA_FLOW_WARNINGS);
    let mut found = Vec::new();
    for finding in inference.type_operations() {
        let (check, message, fixes) = match &finding.kind {
            TypeOperationKind::UselessCast { removable } => (
                Check::UselessCast,
                "No cast needed".to_owned(),
                vec![replace("Remove the cast", *removable, "")],
            ),
            TypeOperationKind::CastNeverSucceeds => (
                Check::CastNeverSucceeds,
                "This cast can never succeed".to_owned(),
                Vec::new(),
            ),
            TypeOperationKind::UncheckedCast { from, to } => (
                Check::UncheckedCast,
                format!(
                    "Unchecked cast of `{}` to `{}`",
                    hir::display_type(db, from),
                    hir::display_type(db, to)
                ),
                Vec::new(),
            ),
            TypeOperationKind::UselessIsCheck { result } => (
                Check::UselessIsCheck,
                format!("Check for instance is always `{result}`"),
                Vec::new(),
            ),
        };
        let needs_data_flow_warnings =
            matches!(check, Check::UselessCast | Check::CastNeverSucceeds);
        if needs_data_flow_warnings && !data_flow_warnings || !config.runs(check) {
            continue;
        }
        found.push(Diagnostic {
            check,
            severity: check.severity(),
            range: finding.range,
            message,
            fixes,
        });
    }
    found
}

/// Redundant `else` branches and conditions a `when` checks twice.
fn when_findings(
    db: &RootDatabase,
    file: SourceFile,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    let inference = hir::infer_file(db, file);
    let text = file.text(db);
    let mut found = Vec::new();
    for finding in inference.when_findings() {
        let (check, message, fixes) = match finding {
            WhenFinding::RedundantElse { entry, .. } => (
                Check::RedundantElseInWhen,
                "`when` is exhaustive so `else` is redundant here",
                vec![replace(
                    "Remove the `else` branch",
                    whole_lines(text, *entry),
                    "",
                )],
            ),
            WhenFinding::DuplicateCondition { .. } => (
                Check::DuplicateBranchCondition,
                "Duplicate branch condition in `when`",
                Vec::new(),
            ),
        };
        if config.runs(check) {
            found.push(Diagnostic {
                check,
                severity: check.severity(),
                range: finding.range(),
                message: message.to_owned(),
                fixes,
            });
        }
    }
    found
}

/// `range`, widened to the lines it's on when nothing else is on them.
fn whole_lines(text: &str, range: TextRange) -> TextRange {
    let line_start = text[..range.start as usize]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let line_end = text[range.end as usize..]
        .find('\n')
        .map_or(text.len(), |newline| range.end as usize + newline + 1);
    let before_blank = text[line_start..range.start as usize].trim().is_empty();
    let after_blank = text[range.end as usize..line_end].trim().is_empty();
    if before_blank && after_blank {
        TextRange::new(line_start, line_end)
    } else {
        range
    }
}

/// Uses of API that needs an opt-in the code doesn't give, as Kotlin words
/// them: `This declaration needs opt-in. Its usage should be marked with
/// '@Marker' or '@OptIn(Marker::class)'` — or the marker's own message.
fn opt_ins(db: &RootDatabase, file: SourceFile, config: &DiagnosticsConfig) -> Vec<Diagnostic> {
    if !config.runs(Check::OptInUsage) && !config.runs(Check::OptInUsageError) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for opt_in in hir::opt_in_uses(db, file) {
        let (check, verb) = match opt_in.level() {
            OptInLevel::Warning => (Check::OptInUsage, "should"),
            OptInLevel::Error => (Check::OptInUsageError, "must"),
        };
        if !config.runs(check) {
            continue;
        }
        let marker = &opt_in.marker_name;
        let message = match opt_in.marker.message.as_deref().map(str::trim) {
            Some(own) if !own.is_empty() => format!("This declaration needs opt-in. {own}"),
            _ => format!(
                "This declaration needs opt-in. Its usage {verb} be marked with `@{marker}` or `@OptIn({marker}::class)`"
            ),
        };
        found.push(Diagnostic {
            check,
            severity: check.severity(),
            range: opt_in.range,
            message,
            fixes: Vec::new(),
        });
    }
    found
}

/// Uses of deprecated declarations, as Kotlin words them: `'name' is
/// deprecated. Message. Replace with 'replacement'.`
fn deprecations(
    db: &RootDatabase,
    file: SourceFile,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    if !config.runs(Check::Deprecation) && !config.runs(Check::DeprecationError) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for deprecated in hir::deprecated_uses(db, file) {
        let check = match deprecated.level {
            DeprecationLevel::Warning => Check::Deprecation,
            DeprecationLevel::Error | DeprecationLevel::Hidden => Check::DeprecationError,
        };
        if !config.runs(check) {
            continue;
        }
        let mut message = format!("`{}` is deprecated.", deprecated.name);
        if let Some(reason) = deprecated.deprecation.message.as_deref().map(str::trim)
            && !reason.is_empty()
        {
            message.push(' ');
            message.push_str(reason);
            if !reason.ends_with('.') {
                message.push('.');
            }
        }
        if let Some(replacement) = &deprecated.deprecation.replace_with {
            message.push_str(&format!(" Replace with `{replacement}`."));
        }
        found.push(Diagnostic {
            check,
            severity: check.severity(),
            range: deprecated.range,
            message,
            fixes: Vec::new(),
        });
    }
    found
}

/// `x!!`, `x?.f`, `x ?: y`, `x == null` where `x` can't be null. The
/// compiler reports a comparison always; the others as data-flow warnings
/// only, from Kotlin 2.3.
fn null_checks(db: &RootDatabase, file: SourceFile, config: &DiagnosticsConfig) -> Vec<Diagnostic> {
    let inference = hir::infer_file(db, file);
    let text = file.text(db);
    let data_flow_warnings = hir::compiler_options(db, file)
        .language_version()
        .is_some_and(|version| version >= DATA_FLOW_WARNINGS);
    let mut found = Vec::new();
    for check in inference.redundant_null_checks() {
        let is_comparison = matches!(check.kind, NullCheckKind::Comparison { .. });
        if !is_comparison && !data_flow_warnings {
            continue;
        }
        let ty = hir::display_type(db, &check.ty);
        let (kind, message, fix) = match &check.kind {
            NullCheckKind::NotNullAssertion => (
                Check::UnnecessaryNotNullAssertion,
                format!("Unnecessary non-null assertion (`!!`) on a value of non-null type `{ty}`"),
                Some(replace("Remove `!!`", check.range, "")),
            ),
            NullCheckKind::SafeCall => (
                Check::UnnecessarySafeCall,
                format!("Unnecessary safe call on a receiver of non-null type `{ty}`"),
                Some(replace("Replace `?.` with `.`", check.range, ".")),
            ),
            NullCheckKind::Elvis { removable } => (
                Check::UselessElvis,
                format!(
                    "Elvis operator (`?:`) always returns the left operand of non-null type `{ty}`"
                ),
                Some(replace("Remove the elvis operator", *removable, "")),
            ),
            NullCheckKind::Comparison { always } => {
                let condition = &text[check.range.start as usize..check.range.end as usize];
                (
                    Check::SenselessComparison,
                    format!("Condition `{condition}` is always `{always}`: `{ty}` isn't nullable"),
                    None,
                )
            }
        };
        if config.runs(kind) {
            found.push(Diagnostic {
                check: kind,
                severity: kind.severity(),
                range: check.range,
                message,
                fixes: fix.into_iter().collect(),
            });
        }
    }
    found
}

/// Locals never read; `var`s never assigned again.
fn local_findings(
    db: &RootDatabase,
    file: SourceFile,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    if !config.runs(Check::UnusedVariable) && !config.runs(Check::CanBeVal) {
        return Vec::new();
    }
    let text = file.text(db);
    let mut found = Vec::new();
    for finding in hir::local_findings(db, file) {
        match finding {
            LocalFinding::Unused { name } if config.runs(Check::UnusedVariable) => {
                let written = &text[name.start as usize..name.end as usize];
                found.push(Diagnostic {
                    check: Check::UnusedVariable,
                    severity: Check::UnusedVariable.severity(),
                    range: name,
                    message: format!("Variable `{written}` is never used"),
                    fixes: Vec::new(),
                });
            }
            LocalFinding::NeverReassigned { keyword } if config.runs(Check::CanBeVal) => {
                found.push(Diagnostic {
                    check: Check::CanBeVal,
                    severity: Check::CanBeVal.severity(),
                    range: keyword,
                    message: "Variable is never assigned again: it can be a `val`".to_owned(),
                    fixes: vec![replace("Change to `val`", keyword, "val")],
                });
            }
            LocalFinding::Unused { .. } | LocalFinding::NeverReassigned { .. } => {}
        }
    }
    found
}

/// A fix that puts `new_text` in place of `range`.
fn replace(label: &str, range: TextRange, new_text: &str) -> Fix {
    Fix {
        label: label.to_owned(),
        edits: vec![TextEdit {
            range,
            new_text: new_text.to_owned(),
        }],
    }
}

/// Adds the missing branches, each `case -> TODO()`, before the `when`'s
/// closing brace.
fn fill_when(parse: &Parse, when: &NonExhaustiveWhen) -> Option<Fix> {
    let start = when.range.start as usize;
    let node = parse
        .root()
        .descendant_for_byte_range(start, start + 1)
        .and_then(|mut node| {
            while node.kind() != "when_expression" {
                node = node.parent()?;
            }
            Some(node)
        })?;
    if !has_token(node, "}") {
        return None;
    }
    let text = parse.text();
    let close = node.end_byte() - 1;
    let line_start = text[..close].rfind('\n').map_or(0, |newline| newline + 1);
    let before_brace = &text[line_start..close];
    let edit = if before_brace.trim().is_empty() {
        // `}` on its own line: branches go above it, indented one level in.
        let indent = format!("{before_brace}    ");
        let branches: String = when
            .missing
            .iter()
            .map(|case| format!("{indent}{case} -> TODO()\n"))
            .collect();
        TextEdit {
            range: TextRange::new(line_start, line_start),
            new_text: branches,
        }
    } else {
        let branches: String = when
            .missing
            .iter()
            .map(|case| format!("; {case} -> TODO()"))
            .collect();
        let trimmed = text[..close].trim_end().len();
        TextEdit {
            range: TextRange::new(trimmed, trimmed),
            new_text: branches,
        }
    };
    Some(Fix {
        label: "Add the missing branches".to_owned(),
        edits: vec![edit],
    })
}
