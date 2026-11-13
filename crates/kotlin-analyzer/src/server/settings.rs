//! The client's settings: `initializationOptions` and
//! `workspace/didChangeConfiguration`.
//!
//! ```json
//! { "diagnostics": { "disabled": ["unused-import"], "experimental": false } }
//! ```

use ide::{Check, DiagnosticsConfig};
use serde_json::Value;

/// The diagnostics settings in `initializationOptions`.
pub(super) fn diagnostics_config(options: Option<&Value>) -> DiagnosticsConfig {
    options
        .and_then(|options| options.get("diagnostics"))
        .map(read)
        .unwrap_or_default()
}

/// The diagnostics settings in a configuration change — under
/// `kotlin-analyzer` or at the top — or `None` when it has none.
pub(super) fn changed_diagnostics_config(settings: &Value) -> Option<DiagnosticsConfig> {
    let ours = settings.get("kotlin-analyzer").unwrap_or(settings);
    ours.get("diagnostics").map(read)
}

fn read(diagnostics: &Value) -> DiagnosticsConfig {
    let disabled = diagnostics
        .get("disabled")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(Check::from_name)
        .collect();
    let experimental = diagnostics
        .get("experimental")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    DiagnosticsConfig {
        disabled,
        experimental,
        only: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_name_checks_to_disable_and_unknown_names_are_ignored() {
        let options = serde_json::json!({
            "diagnostics": { "disabled": ["unused-import", "no-such-check"], "experimental": true }
        });
        let config = diagnostics_config(Some(&options));
        assert_eq!(config.disabled, vec![Check::UnusedImport]);
        assert!(config.experimental);
        assert!(!config.runs(Check::UnusedImport));
        assert!(config.runs(Check::UnresolvedReference));
        let nested =
            serde_json::json!({ "kotlin-analyzer": { "diagnostics": { "disabled": ["syntax"] } } });
        assert_eq!(
            changed_diagnostics_config(&nested).unwrap().disabled,
            vec![Check::Syntax]
        );
        assert!(changed_diagnostics_config(&serde_json::json!({})).is_none());
    }
}
