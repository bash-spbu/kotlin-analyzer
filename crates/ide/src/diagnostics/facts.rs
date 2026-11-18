//! Checks that render facts inference recorded: calls no candidate takes,
//! unsafe calls on nullable receivers, non-exhaustive `when`s, null checks
//! on values that can't be null, unused locals and `var`s that could be
//! `val`s.

use hir::{LocalFinding, NonExhaustiveWhen, NullCheckKind, RootDatabase, SourceFile};
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
    found
}

/// `x!!`, `x?.f`, `x ?: y`, `x == null` where `x` can't be null.
fn null_checks(db: &RootDatabase, file: SourceFile, config: &DiagnosticsConfig) -> Vec<Diagnostic> {
    let inference = hir::infer_file(db, file);
    let text = file.text(db);
    let mut found = Vec::new();
    for check in inference.redundant_null_checks() {
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
    let text = file.text(db);
    let mut found = Vec::new();
    for finding in hir::local_findings(db, file) {
        match finding {
            LocalFinding::Unused { name } if config.runs(Check::UnusedVariable) => {
                let written = &text[name.start as usize..name.end as usize];
                found.push(Diagnostic {
                    check: Check::UnusedVariable,
                    range: name,
                    message: format!("Variable `{written}` is never used"),
                    fixes: Vec::new(),
                });
            }
            LocalFinding::NeverReassigned { keyword } if config.runs(Check::CanBeVal) => {
                found.push(Diagnostic {
                    check: Check::CanBeVal,
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
