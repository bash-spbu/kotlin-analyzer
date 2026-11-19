//! The client's settings: `initializationOptions` and
//! `workspace/didChangeConfiguration`.
//!
//! ```json
//! {
//!   "diagnostics": { "disabled": ["unused-import"], "experimental": false },
//!   "formatting": { "formatter": "ktrs", "style": "kotlinlang", "path": "/path/to/ktrs" },
//!   "codeLens": { "references": true, "tests": true }
//! }
//! ```

use std::path::PathBuf;

use ide::{Check, DiagnosticsConfig};
use serde_json::Value;

use super::code_lens::CodeLensConfig;
use super::formatting::{Formatter, FormatterSetup, FormattingConfig, Program, Style};

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

/// The code lens settings in `initializationOptions`.
pub(super) fn code_lens_config(options: Option<&Value>) -> CodeLensConfig {
    options
        .and_then(|options| options.get("codeLens"))
        .map(read_code_lens)
        .unwrap_or_default()
}

/// The code lens settings in a configuration change, if it has them.
pub(super) fn changed_code_lens_config(settings: &Value) -> Option<CodeLensConfig> {
    let ours = settings.get("kotlin-analyzer").unwrap_or(settings);
    ours.get("codeLens").map(read_code_lens)
}

/// `references` and `tests`, each on unless `false`.
fn read_code_lens(code_lens: &Value) -> CodeLensConfig {
    let enabled = |key: &str| code_lens.get(key).and_then(Value::as_bool).unwrap_or(true);
    CodeLensConfig {
        references: enabled("references"),
        tests: enabled("tests"),
    }
}

/// The formatting settings in `initializationOptions`.
pub(super) fn formatting_config(options: Option<&Value>) -> FormattingConfig {
    options
        .and_then(|options| options.get("formatting"))
        .map(read_formatting)
        .unwrap_or_default()
}

/// The formatting settings in a configuration change, if it has them.
pub(super) fn changed_formatting_config(settings: &Value) -> Option<FormattingConfig> {
    let ours = settings.get("kotlin-analyzer").unwrap_or(settings);
    ours.get("formatting").map(read_formatting)
}

/// `formatter`: `ktrs` (the default), `ktfmt` or `off`; `style`:
/// `kotlinlang` (the default), `meta` or `google`; `path`: the formatter's
/// executable or jar, else its name on `PATH`. A value not understood is
/// the default.
fn read_formatting(formatting: &Value) -> FormattingConfig {
    let setting = |key: &str| formatting.get(key).and_then(Value::as_str);
    let formatter = match setting("formatter") {
        Some("off") => return FormattingConfig::Off,
        Some(name) => Formatter::from_setting(name).unwrap_or(Formatter::Ktrs),
        None => Formatter::Ktrs,
    };
    let style = setting("style")
        .and_then(Style::from_setting)
        .unwrap_or_default();
    let program = setting("path")
        .filter(|path| !path.is_empty())
        .map_or(Program::OnPath, |path| Program::at(PathBuf::from(path)));
    FormattingConfig::On(FormatterSetup {
        formatter,
        style,
        program,
    })
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

    #[test]
    fn formatting_defaults_to_ktrs_in_kotlinlang_style_on_path() {
        assert_eq!(formatting_config(None), FormattingConfig::default());
        let FormattingConfig::On(default) = FormattingConfig::default() else {
            panic!("formatting is on by default");
        };
        assert_eq!(default.formatter, Formatter::Ktrs);
        assert_eq!(default.style, Style::Kotlinlang);
        assert_eq!(default.program, Program::OnPath);

        let options = serde_json::json!({
            "formatting": { "formatter": "ktfmt", "style": "google", "path": "/tools/ktfmt.jar" }
        });
        assert_eq!(
            formatting_config(Some(&options)),
            FormattingConfig::On(FormatterSetup {
                formatter: Formatter::Ktfmt,
                style: Style::Google,
                program: Program::Jar(PathBuf::from("/tools/ktfmt.jar")),
            })
        );
        let unknown = serde_json::json!({
            "formatting": { "formatter": "prettier", "style": "tabs", "path": "" }
        });
        assert_eq!(
            formatting_config(Some(&unknown)),
            FormattingConfig::default()
        );
        let off =
            serde_json::json!({ "kotlin-analyzer": { "formatting": { "formatter": "off" } } });
        assert_eq!(changed_formatting_config(&off), Some(FormattingConfig::Off));
        assert_eq!(changed_formatting_config(&serde_json::json!({})), None);
    }

    #[test]
    fn code_lenses_are_each_on_unless_turned_off() {
        assert_eq!(code_lens_config(None), CodeLensConfig::default());
        let options = serde_json::json!({ "codeLens": { "tests": false } });
        assert_eq!(
            code_lens_config(Some(&options)),
            CodeLensConfig {
                references: true,
                tests: false
            }
        );
        let changed =
            serde_json::json!({ "kotlin-analyzer": { "codeLens": { "references": false } } });
        assert_eq!(
            changed_code_lens_config(&changed),
            Some(CodeLensConfig {
                references: false,
                tests: true
            })
        );
        assert_eq!(changed_code_lens_config(&serde_json::json!({})), None);
    }
}
