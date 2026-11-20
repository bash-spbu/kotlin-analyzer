//! Registers kotlin-analyzer as a language server for Kotlin and Java in
//! Zed. The binary is the first of: `lsp.kotlin-analyzer.binary.path` from
//! the settings, `kotlin-analyzer` on the PATH, this extension's release
//! (`release::SERVER_VERSION`) downloaded once into the extension's work
//! directory with its checksum verified. `initialization_options` and
//! `settings` under `lsp.kotlin-analyzer` are passed through, with the
//! formatter they choose (`formatting.formatter`: ktrs unless `ktfmt` or
//! `off`) added as `formatting.path` unless they give one: the formatter on
//! the PATH, else its pinned release, downloaded once like the server's.
//! Code lenses show references, what a member overrides and what implements
//! it, with `editor.action.showReferences`, which Zed runs itself (with
//! `"code_lens": "on"` in Zed's settings); the test lenses are off unless
//! they say: Zed can't run their command, and tests run from the gutter
//! (`runnables.scm`).
//! The same binary is the debugger (`debugging`).

mod debugging;
mod formatter;
mod labels;
mod release;

use zed_extension_api::settings::{ContextServerSettings, LspSettings};
use zed_extension_api::{
    self as zed, Architecture, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario,
    DebugTaskDefinition, DownloadedFileType, LanguageServerId, LanguageServerInstallationStatus,
    Os, Result, StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest,
    TaskTemplate, Worktree,
};

use debugging::RequestKind;
use formatter::{Formatter, FormatterAsset, Packaging};
use labels::{Declared, Label};
use release::{Asset, Machine, System};
use zed::serde_json::Value;
use zed_extension_api::lsp::{Completion, CompletionKind, Symbol, SymbolKind};

const SERVER: &str = "kotlin-analyzer";

struct KotlinAnalyzer {
    /// The downloaded release, once found or installed this session.
    downloaded: Option<String>,
    /// The binary the language server runs this session: the MCP server
    /// runs the same.
    server_binary: Option<String>,
}

impl zed::Extension for KotlinAnalyzer {
    fn new() -> Self {
        KotlinAnalyzer {
            downloaded: None,
            server_binary: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.binary);
        let configured = binary.as_ref().and_then(|binary| binary.path.clone());
        let command = match configured.or_else(|| worktree.which(SERVER)) {
            Some(command) => command,
            None => self.downloaded_release(Some(language_server_id))?,
        };
        self.server_binary = Some(absolute(&command));
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
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        let settings = LspSettings::for_worktree(SERVER, worktree).ok();
        let options = settings
            .as_ref()
            .and_then(|settings| settings.initialization_options.clone());
        let options = with_zed_code_lenses(Some(
            options.unwrap_or_else(|| Value::Object(Default::default())),
        ));
        Ok(self.with_formatter(options, settings.as_ref(), language_server_id, worktree))
    }

    /// The MCP server for agents (`kotlin-analyzer mcp`): the language
    /// server's binary — `context_servers.kotlin-analyzer.command` in the
    /// settings, else the one the language server runs, else the release.
    fn context_server_command(
        &mut self,
        _context_server_id: &zed::ContextServerId,
        project: &zed::Project,
    ) -> Result<zed::Command> {
        let configured = ContextServerSettings::for_project(SERVER, project)
            .ok()
            .and_then(|settings| settings.command);
        let configured_path = configured.as_ref().and_then(|command| command.path.clone());
        let command = match configured_path.or_else(|| self.server_binary.clone()) {
            Some(command) => command,
            None => absolute(&self.downloaded_release(None)?),
        };
        let arguments = configured
            .as_ref()
            .and_then(|command| command.arguments.clone())
            .unwrap_or_else(|| vec!["mcp".to_owned()]);
        let environment = configured
            .and_then(|command| command.env)
            .map(|environment| environment.into_iter().collect())
            .unwrap_or_default();
        Ok(zed::Command {
            command,
            args: arguments,
            env: environment,
        })
    }

    /// The debugger: `kotlin-analyzer dap` of the binary the language
    /// server runs (or the one `dap.Kotlin.binary` gives), in the
    /// worktree.
    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != debugging::ADAPTER {
            return Err(format!("no debug adapter `{adapter_name}` here"));
        }
        let configuration: Value = zed::serde_json::from_str(&config.config)
            .map_err(|error| format!("the configuration isn't JSON: {error}"))?;
        let request = self.dap_request_kind(adapter_name, configuration)?;
        let command = match user_provided_debug_adapter_path {
            Some(path) => path,
            None => self.server_binary_for(worktree)?,
        };
        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments: vec!["dap".to_owned()],
            envs: worktree.shell_env(),
            cwd: Some(worktree.root_path()),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: config.config,
                request,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        Ok(match debugging::request_kind(&config)? {
            RequestKind::Launch => StartDebuggingRequestArgumentsRequest::Launch,
            RequestKind::Attach => StartDebuggingRequestArgumentsRequest::Attach,
        })
    }

    /// A program from the "new session" form: `java …` or Gradle.
    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        let DebugRequest::Launch(launch) = config.request else {
            return Err(
                "kotlin-analyzer attaches by address, not by process: use a debug.json \
                 configuration with `\"request\": \"attach\"` and `port`"
                    .to_owned(),
            );
        };
        let configuration = debugging::program_configuration(
            &launch.program,
            &launch.args,
            launch.cwd.as_deref(),
            &launch.envs,
        );
        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: configuration.to_string(),
            tcp_connection: None,
        })
    }

    /// A test task of a gutter runnable, debugged: the same test, under
    /// the debugger.
    fn dap_locator_create_scenario(
        &mut self,
        locator_name: String,
        build_task: TaskTemplate,
        resolved_label: String,
        debug_adapter_name: String,
    ) -> Option<DebugScenario> {
        let is_ours =
            locator_name == debugging::LOCATOR && debug_adapter_name == debugging::ADAPTER;
        if !is_ours {
            return None;
        }
        let configuration = debugging::test_configuration(&build_task.command)?;
        Some(DebugScenario {
            label: resolved_label,
            adapter: debug_adapter_name,
            build: None,
            config: configuration.to_string(),
            tcp_connection: None,
        })
    }

    fn label_for_completion(
        &self,
        _language_server_id: &LanguageServerId,
        completion: Completion,
    ) -> Option<zed::CodeLabel> {
        let declared = match completion.kind? {
            CompletionKind::Function | CompletionKind::Method | CompletionKind::Constructor => {
                Declared::Function
            }
            CompletionKind::Property
            | CompletionKind::Field
            | CompletionKind::Variable
            | CompletionKind::Constant => Declared::Value,
            CompletionKind::Class | CompletionKind::Interface | CompletionKind::Enum => {
                Declared::Type
            }
            CompletionKind::EnumMember => Declared::EnumEntry,
            _ => return None,
        };
        let details = completion.label_details.as_ref();
        let label = labels::completion_label(
            declared,
            &completion.label,
            details.and_then(|details| details.detail.as_deref()),
            details.and_then(|details| details.description.as_deref()),
        )?;
        Some(code_label(label))
    }

    fn label_for_symbol(
        &self,
        _language_server_id: &LanguageServerId,
        symbol: Symbol,
    ) -> Option<zed::CodeLabel> {
        let declared = match symbol.kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
                Declared::Function
            }
            SymbolKind::Property
            | SymbolKind::Field
            | SymbolKind::Variable
            | SymbolKind::Constant => Declared::Value,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Struct => {
                Declared::Type
            }
            SymbolKind::EnumMember => Declared::EnumEntry,
            _ => return None,
        };
        Some(code_label(labels::symbol_label(declared, &symbol.name)?))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        let settings = LspSettings::for_worktree(SERVER, worktree).ok();
        let configuration = settings
            .as_ref()
            .and_then(|settings| settings.settings.clone());
        let configuration = with_zed_code_lenses(configuration);
        Ok(self.with_formatter(
            configuration,
            settings.as_ref(),
            language_server_id,
            worktree,
        ))
    }
}

impl KotlinAnalyzer {
    /// The server's binary for `worktree`, as the language server finds it:
    /// the settings' path, the PATH, else the release.
    fn server_binary_for(&mut self, worktree: &Worktree) -> Result<String> {
        let configured = LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.binary)
            .and_then(|binary| binary.path);
        let command = match configured.or_else(|| worktree.which(SERVER)) {
            Some(command) => command,
            None => self.downloaded_release(None)?,
        };
        Ok(absolute(&command))
    }

    /// The release binary's path, downloaded first if it isn't there — with
    /// the download shown as the language server's status when it's for
    /// one.
    fn downloaded_release(
        &mut self,
        language_server_id: Option<&LanguageServerId>,
    ) -> Result<String> {
        if let Some(path) = &self.downloaded {
            if is_file(path) {
                return Ok(path.clone());
            }
        }
        let asset = this_machines_asset()?;
        let binary = asset.binary();
        if !is_file(&binary) {
            let status = |status: &LanguageServerInstallationStatus| {
                if let Some(language_server_id) = language_server_id {
                    zed::set_language_server_installation_status(language_server_id, status);
                }
            };
            status(&LanguageServerInstallationStatus::Downloading);
            let installed = install(&asset);
            if let Err(error) = &installed {
                status(&LanguageServerInstallationStatus::Failed(error.clone()));
            }
            installed?;
            status(&LanguageServerInstallationStatus::None);
            remove_other_versions("kotlin-analyzer-", &asset.directory());
        }
        self.downloaded = Some(binary.clone());
        Ok(binary)
    }
}

impl KotlinAnalyzer {
    /// `options` with the formatter the settings choose and its path under
    /// `formatting`, unless they give a path or turn formatting off. A
    /// formatter that can't be had leaves them as they are: the server
    /// then looks for it on its PATH, and says so when it formats.
    fn with_formatter(
        &mut self,
        options: Option<Value>,
        settings: Option<&LspSettings>,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Option<Value> {
        let path_given = settings.and_then(|settings| formatting_setting(settings, "path"));
        if path_given.is_some() {
            return options;
        }
        let chosen = settings.and_then(|settings| formatting_setting(settings, "formatter"));
        let Some(formatter) = Formatter::from_setting(chosen.as_deref()) else {
            return options;
        };
        let mut options = options.unwrap_or_else(|| Value::Object(Default::default()));
        let formatting_is_settable = options.is_object()
            && options
                .get("formatting")
                .is_none_or(|formatting| formatting.is_object());
        if !formatting_is_settable {
            return Some(options);
        }
        let Some(program) = formatter_program(formatter, language_server_id, worktree) else {
            return Some(options);
        };
        options["formatting"]["formatter"] = Value::from(formatter.name());
        options["formatting"]["path"] = Value::from(program);
        Some(options)
    }
}

/// `options` with the code lenses Zed can run: those showing locations
/// with `editor.action.showReferences` (Zed runs it itself), the test ones
/// off (Zed runs only rust-analyzer's other commands). What `options` say
/// about `codeLens` stays; `None` (settings that don't change) stays.
fn with_zed_code_lenses(options: Option<Value>) -> Option<Value> {
    let mut options = options?;
    let Some(object) = options.as_object_mut() else {
        return Some(options);
    };
    let code_lens = object
        .entry("codeLens")
        .or_insert_with(|| Value::Object(Default::default()));
    if let Some(code_lens) = code_lens.as_object_mut() {
        code_lens
            .entry("locationsCommand")
            .or_insert_with(|| Value::from("editor.action.showReferences"));
        code_lens.entry("tests").or_insert(Value::Bool(false));
    }
    Some(options)
}

/// Zed's label: the shown part of the code, then where it's declared
/// coloured as a comment.
fn code_label(label: Label) -> zed::CodeLabel {
    let mut spans = vec![zed::CodeLabelSpan::code_range(label.shown.clone())];
    if let Some(description) = label.description {
        spans.push(zed::CodeLabelSpan::literal(
            format!("  {description}"),
            Some("comment".to_owned()),
        ));
    }
    zed::CodeLabel {
        code: label.code,
        spans,
        filter_range: label.filter.into(),
    }
}

/// A `formatting` setting as text: from `settings`, else from
/// `initialization_options`.
fn formatting_setting(settings: &LspSettings, key: &str) -> Option<String> {
    [
        settings.settings.as_ref(),
        settings.initialization_options.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find_map(|options| {
        options
            .get("formatting")?
            .get(key)?
            .as_str()
            .map(str::to_owned)
    })
}

/// The formatter's absolute path: on the worktree's PATH, else its pinned
/// release, downloaded first if it isn't there.
fn formatter_program(
    formatter: Formatter,
    language_server_id: &LanguageServerId,
    worktree: &Worktree,
) -> Option<String> {
    if let Some(on_path) = worktree.which(formatter.name()) {
        return Some(on_path);
    }
    let (system, machine) = this_machine().ok()?;
    let asset = FormatterAsset::for_machine(formatter, system, machine);
    let program = asset.program();
    if !is_file(&program) {
        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::Downloading,
        );
        let status = match install_formatter(&asset) {
            Ok(()) => LanguageServerInstallationStatus::None,
            Err(error) => LanguageServerInstallationStatus::Failed(format!(
                "downloading the {} formatter: {error}",
                formatter.name()
            )),
        };
        zed::set_language_server_installation_status(language_server_id, &status);
        if !is_file(&program) {
            return None;
        }
        remove_other_versions(&format!("{}-", formatter.name()), &asset.directory());
    }
    // The server runs in the worktree: it needs the path from the root.
    let work_directory = std::env::current_dir().ok()?;
    Some(work_directory.join(program).to_string_lossy().into_owned())
}

/// Downloads `asset` into a scratch directory, and writes the program to
/// `asset.program()` only once its checksum is the pinned one.
fn install_formatter(asset: &FormatterAsset) -> Result<()> {
    let scratch = format!("{}.download", asset.directory());
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|error| format!("creating {scratch}: {error}"))?;
    let downloaded_path = format!("{scratch}/{}", asset.file_name);
    let downloaded = zed::download_file(
        &asset.url,
        &downloaded_path,
        DownloadedFileType::Uncompressed,
    )
    .and_then(|()| std::fs::read(&downloaded_path).map_err(|error| error.to_string()));
    let _ = std::fs::remove_dir_all(&scratch);
    let program = asset.verified_program(&downloaded?)?;
    std::fs::create_dir_all(asset.directory()).map_err(|error| error.to_string())?;
    std::fs::write(asset.program(), program).map_err(|error| error.to_string())?;
    match asset.packaging {
        Packaging::TarGz { .. } => zed::make_file_executable(&asset.program()),
        Packaging::Jar => Ok(()),
    }
}

fn this_machines_asset() -> Result<Asset> {
    let (system, machine) = this_machine()?;
    Ok(Asset::for_machine(system, machine))
}

fn this_machine() -> Result<(System, Machine)> {
    let (os, architecture) = zed::current_platform();
    let system = match os {
        Os::Mac => System::Mac,
        Os::Linux => System::Linux,
        Os::Windows => return Err(no_release("Windows")),
    };
    let machine = match architecture {
        Architecture::Aarch64 => Machine::Aarch64,
        Architecture::X8664 => Machine::X86_64,
        Architecture::X86 => return Err(no_release("32-bit x86")),
    };
    Ok((system, machine))
}

fn no_release(platform: &str) -> String {
    format!(
        "no {SERVER} release for {platform}: build it (cargo install --path crates/kotlin-analyzer) \
         and set lsp.{SERVER}.binary.path"
    )
}

/// Downloads `asset` and its checksum into a scratch directory, and writes
/// the binary to `asset.binary()` only once they agree.
fn install(asset: &Asset) -> Result<()> {
    let scratch = format!("{}.download", asset.directory());
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|error| format!("creating {scratch}: {error}"))?;
    let packed_path = format!("{scratch}/{}", asset.file_name());
    let checksum_path = format!("{packed_path}.sha256");
    zed::download_file(&asset.url(), &packed_path, DownloadedFileType::Uncompressed)?;
    zed::download_file(
        &asset.checksum_url(),
        &checksum_path,
        DownloadedFileType::Uncompressed,
    )?;
    let packed = std::fs::read(&packed_path).map_err(|error| error.to_string())?;
    let checksum_file =
        std::fs::read_to_string(&checksum_path).map_err(|error| error.to_string())?;
    let verified = release::verified_binary(&packed, &checksum_file, &asset.file_name());
    let _ = std::fs::remove_dir_all(&scratch);
    let binary = verified?;
    std::fs::create_dir_all(asset.directory()).map_err(|error| error.to_string())?;
    std::fs::write(asset.binary(), binary).map_err(|error| error.to_string())?;
    zed::make_file_executable(&asset.binary())
}

/// Drops the downloads of other versions: the directories named
/// `<prefix><version>` but `current`.
fn remove_other_versions(prefix: &str, current: &str) {
    let Ok(entries) = std::fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_other_release = name.starts_with(prefix) && name != current;
        if is_other_release {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `path` from the extension's work directory: what a process Zed starts
/// elsewhere can run.
fn absolute(path: &str) -> String {
    let is_relative = std::path::Path::new(path).is_relative() && path.contains('/');
    match std::env::current_dir() {
        Ok(work_directory) if is_relative => {
            work_directory.join(path).to_string_lossy().into_owned()
        }
        _ => path.to_owned(),
    }
}

fn is_file(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

zed::register_extension!(KotlinAnalyzer);

#[cfg(test)]
mod tests {
    use super::with_zed_code_lenses;
    use zed_extension_api::serde_json::json;

    #[test]
    fn code_lenses_show_locations_with_zeds_command_and_tests_stay_off() {
        assert_eq!(
            with_zed_code_lenses(Some(json!({}))),
            Some(json!({
                "codeLens": { "locationsCommand": "editor.action.showReferences", "tests": false }
            }))
        );
        assert_eq!(
            with_zed_code_lenses(Some(
                json!({ "codeLens": { "hierarchy": false, "tests": true } })
            )),
            Some(json!({
                "codeLens": {
                    "hierarchy": false,
                    "tests": true,
                    "locationsCommand": "editor.action.showReferences",
                }
            }))
        );
        assert_eq!(
            with_zed_code_lenses(None),
            None,
            "no settings: nothing changes"
        );
    }
}
