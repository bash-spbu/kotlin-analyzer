//! Registers kotlin-analyzer as a language server for Kotlin and Java in
//! Zed. The binary is the first of: `lsp.kotlin-analyzer.binary.path` from
//! the settings, `kotlin-analyzer` on the PATH, this extension's release
//! (`release::SERVER_VERSION`) downloaded once into the extension's work
//! directory with its checksum verified. `initialization_options` and
//! `settings` under `lsp.kotlin-analyzer` are passed through.

mod release;

use zed_extension_api::settings::LspSettings;
use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

use release::{Asset, Machine, System};

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
            remove_other_versions(&asset);
        }
        self.downloaded = Some(binary.clone());
        Ok(binary)
    }
}

fn this_machines_asset() -> Result<Asset> {
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
    Ok(Asset::for_machine(system, machine))
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

/// Drops the releases of earlier (or later) versions of the extension.
fn remove_other_versions(asset: &Asset) {
    let Ok(entries) = std::fs::read_dir(".") else {
        return;
    };
    let current = asset.directory();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_other_release = name.starts_with("kotlin-analyzer-") && name != current;
        if is_other_release {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn is_file(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

zed::register_extension!(KotlinAnalyzer);
