//! The client's settings: `initializationOptions` and
//! `workspace/didChangeConfiguration`.
//!
//! ```json
//! { "diagnostics": { "disabled": ["unused-import"], "experimental": false } }
//! ```

use ide::{Check, DiagnosticsConfig};
use serde_json::Value;

/// How much the log writes as it happens: `log` in `initializationOptions`
/// (`info`, `debug`), else `KOTLIN_ANALYZER_LOG`, else `info`.
pub(super) fn log_level(options: Option<&Value>) -> crate::log::Level {
    let from_options = options
        .and_then(|options| options.get("log"))
        .and_then(Value::as_str)
        .and_then(crate::log::Level::from_setting);
    let from_environment = std::env::var("KOTLIN_ANALYZER_LOG")
        .ok()
        .and_then(|setting| crate::log::Level::from_setting(&setting));
    from_options
        .or(from_environment)
        .unwrap_or(crate::log::Level::Info)
}

/// The log level in a configuration change, if it has one.
pub(super) fn changed_log_level(settings: &Value) -> Option<crate::log::Level> {
    let ours = settings.get("kotlin-analyzer").unwrap_or(settings);
    ours.get("log")
        .and_then(Value::as_str)
        .and_then(crate::log::Level::from_setting)
}

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
