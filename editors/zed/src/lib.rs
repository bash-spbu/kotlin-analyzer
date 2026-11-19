//! Registers kotlin-analyzer as a language server for Kotlin and Java in
//! Zed. The binary is the first of: `lsp.kotlin-analyzer.binary.path` from
//! the settings, `kotlin-analyzer` on the PATH, this extension's release
//! (`release::SERVER_VERSION`) downloaded once into the extension's work
//! directory with its checksum verified. `initialization_options` and
//! `settings` under `lsp.kotlin-analyzer` are passed through, with the
//! formatter they choose (`formatting.formatter`: ktrs unless `ktfmt` or
//! `off`) added as `formatting.path` unless they give one: the formatter on
//! the PATH, else its pinned release, downloaded once like the server's.
//! Code lenses are off unless they say (`codeLens`): Zed can't run their
//! commands, and tests run from the gutter instead (`runnables.scm`).

mod formatter;
mod release;

use zed_extension_api::settings::LspSettings;
use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

use formatter::{Formatter, FormatterAsset, Packaging};
use release::{Asset, Machine, System};
use zed::serde_json::Value;

const SERVER: &str = "kotlin-analyzer";

struct KotlinAnalyzer {
    /// The downloaded release, once found or installed this session.
    downloaded: Option<String>,
}

impl zed::Extension for KotlinAnalyzer {
    fn new() -> Self {
        KotlinAnalyzer { downloaded: None }
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
            None => self.downloaded_release(language_server_id)?,
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
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<Value>> {
        let settings = LspSettings::for_worktree(SERVER, worktree).ok();
        let options = settings
            .as_ref()
            .and_then(|settings| settings.initialization_options.clone());
        let options = with_code_lenses_off_by_default(options);
        Ok(self.with_formatter(options, settings.as_ref(), language_server_id, worktree))
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
        Ok(self.with_formatter(
            configuration,
            settings.as_ref(),
            language_server_id,
            worktree,
        ))
    }
}

impl KotlinAnalyzer {
    /// The release binary's path, downloading it first if it isn't there.
    fn downloaded_release(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        if let Some(path) = &self.downloaded {
            if is_file(path) {
                return Ok(path.clone());
            }
        }
        let asset = this_machines_asset()?;
        let binary = asset.binary();
        if !is_file(&binary) {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );
            let installed = install(&asset);
            if let Err(error) = &installed {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &LanguageServerInstallationStatus::Failed(error.clone()),
                );
            }
            installed?;
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::None,
            );
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

/// `options` with `codeLens` off when they don't set it: the lenses' commands
/// are the client's to run, and Zed runs only rust-analyzer's.
fn with_code_lenses_off_by_default(options: Option<Value>) -> Option<Value> {
    let mut options = options.unwrap_or_else(|| Value::Object(Default::default()));
    if let Some(object) = options.as_object_mut() {
        object
            .entry("codeLens")
            .or_insert_with(|| zed::serde_json::json!({ "references": false, "tests": false }));
    }
    Some(options)
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

fn is_file(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

zed::register_extension!(KotlinAnalyzer);
