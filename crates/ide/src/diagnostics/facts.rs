//! Checks that render facts inference recorded: calls no candidate takes,
//! unsafe calls on nullable receivers, non-exhaustive `when`s.

use hir::{NonExhaustiveWhen, RootDatabase, SourceFile};
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
    found
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
