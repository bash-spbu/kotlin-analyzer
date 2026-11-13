//! Registers kotlin-analyzer as a language server for Kotlin and Java in
//! Zed. The binary is `lsp.kotlin-analyzer.binary.path` from the settings,
//! else `kotlin-analyzer` on the PATH (`cargo install --path
//! crates/kotlin-analyzer`); `lsp.kotlin-analyzer.initialization_options`
//! and `.settings` are passed through.

use zed_extension_api::settings::LspSettings;
use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

const SERVER: &str = "kotlin-analyzer";

struct KotlinAnalyzer;

impl zed::Extension for KotlinAnalyzer {
    fn new() -> Self {
        KotlinAnalyzer
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.binary);
        let configured = binary.as_ref().and_then(|binary| binary.path.clone());
        let command = match configured.or_else(|| worktree.which(SERVER)) {
            Some(command) => command,
            None => {
                return Err(format!(
                    "{SERVER} isn't on the PATH: install it with `cargo install --path crates/kotlin-analyzer` \
                     in the kotlin-analyzer repository, or set lsp.{SERVER}.binary.path"
                ))
            }
        };
        let arguments = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        let mut environment = worktree.shell_env();
        if let Some(extra) = binary.and_then(|binary| binary.env) {
            environment.extend(extra);
        }
        Ok(zed::Command {
            command,
            args: arguments,
            env: environment,
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.initialization_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.settings))
    }
}

zed::register_extension!(KotlinAnalyzer);
