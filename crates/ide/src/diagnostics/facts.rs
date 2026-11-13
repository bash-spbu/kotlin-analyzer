//! Checks that render facts inference recorded: calls no candidate takes,
//! unsafe calls on nullable receivers, non-exhaustive `when`s.

use hir::{RootDatabase, SourceFile};

use super::{Check, Diagnostic, DiagnosticsConfig};

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
                fixes: Vec::new(),
            });
        }
    }
    found
}
