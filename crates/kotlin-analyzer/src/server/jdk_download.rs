//! Downloading a JDK into `~/.jdks` when the machine has none
//! (`docs/spec/jdk.md`): the questions asking which, and the download —
//! found with the Foojay Disco API (Corretto: with Amazon's own latest
//! addresses), fetched with `curl`, its SHA-256 checked, unpacked with
//! `tar` and moved into place.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use lsp_types::{MessageActionItem, MessageType, ShowMessageRequestParams};
use project_model::jdk::jdk_home_in;
use sha2::{Digest, Sha256};

/// The vendors offered: those whose packages have a SHA-256 to check them
/// against — from the Foojay API, a file it links, GitHub's digest of a
/// release asset, or Amazon's latest checksum for Corretto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vendor {
    Temurin,
    Zulu,
    Corretto,
    Liberica,
    GraalVmCommunity,
    Microsoft,
    OracleOpenJdk,
}

impl Vendor {
    const ALL: [Vendor; 7] = [
        Vendor::Temurin,
        Vendor::Zulu,
        Vendor::Corretto,
        Vendor::Liberica,
        Vendor::GraalVmCommunity,
        Vendor::Microsoft,
        Vendor::OracleOpenJdk,
    ];

    /// Its name on a button.
    fn label(self) -> &'static str {
        match self {
            Vendor::Temurin => "Eclipse Temurin",
            Vendor::Zulu => "Azul Zulu",
            Vendor::Corretto => "Amazon Corretto",
            Vendor::Liberica => "BellSoft Liberica",
            Vendor::GraalVmCommunity => "GraalVM Community",
            Vendor::Microsoft => "Microsoft",
            Vendor::OracleOpenJdk => "Oracle OpenJDK",
        }
    }

    /// Its name in the Foojay API, in the command's arguments and the log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Vendor::Temurin => "temurin",
            Vendor::Zulu => "zulu",
            Vendor::Corretto => "corretto",
            Vendor::Liberica => "liberica",
            Vendor::GraalVmCommunity => "graalvm_community",
            Vendor::Microsoft => "microsoft",
            Vendor::OracleOpenJdk => "oracle_open_jdk",
        }
    }

    /// Its name in the installed JDK's directory (`temurin-21.0.5`), as
    /// IntelliJ IDEA names it.
    fn directory_prefix(self) -> &'static str {
        match self {
            Vendor::GraalVmCommunity => "graalvm-community",
            Vendor::OracleOpenJdk => "openjdk",
            other => other.name(),
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Vendor> {
        Vendor::ALL.into_iter().find(|vendor| vendor.name() == name)
    }

    fn from_label(label: &str) -> Option<Vendor> {
        Vendor::ALL
            .into_iter()
            .find(|vendor| vendor.label() == label)
    }
}

/// The long-term-support releases offered, the one Gradle runs on most
/// widely first.
const LTS_RELEASES: [u32; 3] = [21, 25, 17];

const NOT_NOW: &str = "Not now";
const CANCEL: &str = "Cancel";

/// A question asked about downloading a JDK, waiting for its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JdkQuestion {
    /// Which vendor; then which release, the project's first.
    Vendor { project_release: Option<u32> },
    /// Which of `offered` releases of `vendor`.
    Release { vendor: Vendor, offered: Vec<u32> },
}

/// What the user answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JdkAnswer {
    Vendor(Vendor),
    Release(Vendor, u32),
    /// *Not now*, *Cancel*, or the question dismissed.
    Declined,
}

impl JdkQuestion {
    /// The question after `vendor` is chosen.
    pub(crate) fn release_of(vendor: Vendor, project_release: Option<u32>) -> JdkQuestion {
        let mut offered: Vec<u32> = project_release.into_iter().collect();
        offered.extend(
            LTS_RELEASES
                .into_iter()
                .filter(|release| Some(*release) != project_release),
        );
        JdkQuestion::Release { vendor, offered }
    }

    pub(crate) fn params(&self) -> ShowMessageRequestParams {
        let (message, mut titles) = match self {
            JdkQuestion::Vendor { .. } => (
                "kotlin-analyzer found no JDK (JAVA_HOME, java on PATH, ~/.jdks): \
                 Gradle can't export the project and the JDK's classes stay unknown. \
                 Download one into ~/.jdks? From which vendor?"
                    .to_owned(),
                Vendor::ALL
                    .iter()
                    .map(|vendor| vendor.label().to_owned())
                    .collect::<Vec<_>>(),
            ),
            JdkQuestion::Release { vendor, offered } => (
                format!("Which Java version of {}?", vendor.label()),
                offered
                    .iter()
                    .enumerate()
                    .map(|(index, release)| {
                        release_title(*release, index == 0 && !is_lts(*release))
                    })
                    .collect(),
            ),
        };
        titles.push(
            match self {
                JdkQuestion::Vendor { .. } => NOT_NOW,
                JdkQuestion::Release { .. } => CANCEL,
            }
            .to_owned(),
        );
        ShowMessageRequestParams {
            typ: MessageType::WARNING,
            message,
            actions: Some(
                titles
                    .into_iter()
                    .map(|title| MessageActionItem {
                        title,
                        properties: Default::default(),
                    })
                    .collect(),
            ),
        }
    }

    /// The answer the button titled `chosen` (`None`: dismissed) gives.
    pub(crate) fn answer(&self, chosen: Option<&str>) -> JdkAnswer {
        let Some(chosen) = chosen else {
            return JdkAnswer::Declined;
        };
        match self {
            JdkQuestion::Vendor { .. } => {
                Vendor::from_label(chosen).map_or(JdkAnswer::Declined, JdkAnswer::Vendor)
            }
            JdkQuestion::Release { vendor, offered } => offered
                .iter()
                .find(|release| chosen.split(' ').next() == Some(release.to_string().as_str()))
                .map_or(JdkAnswer::Declined, |release| {
                    JdkAnswer::Release(*vendor, *release)
                }),
        }
    }
}

fn is_lts(release: u32) -> bool {
    LTS_RELEASES.contains(&release)
}

/// `21 (LTS)`, or `11 (the project's)` for a release offered as the
/// project's that isn't among the long-term ones offered.
fn release_title(release: u32, is_the_projects: bool) -> String {
    if is_the_projects {
        format!("{release} (the project's)")
    } else if is_lts(release) {
        format!("{release} (LTS)")
    } else {
        release.to_string()
    }
}

/// The OS and architecture as the Foojay API names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Platform {
    operating_system: &'static str,
    architecture: &'static str,
    /// Linux's C library (`glibc`); `None` elsewhere.
    lib_c: Option<&'static str>,
}

impl Platform {
    fn current() -> Option<Platform> {
        let operating_system = match std::env::consts::OS {
            "macos" => "macos",
            "linux" => "linux",
            _ => return None,
        };
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x64",
            _ => return None,
        };
        let lib_c = (operating_system == "linux").then_some("glibc");
        Some(Platform {
            operating_system,
            architecture,
            lib_c,
        })
    }
}

const FOOJAY_PACKAGES: &str = "https://api.foojay.io/disco/v3.0/packages";

/// The query for the latest GA `.tar.gz` JDK of `vendor` and `release`.
fn packages_url(vendor: Vendor, release: u32, platform: Platform) -> String {
    let mut url = format!(
        "{FOOJAY_PACKAGES}?version={release}&distribution={}&architecture={}\
         &operating_system={}&archive_type=tar.gz&package_type=jdk\
         &release_status=ga&latest=available&javafx_bundled=false\
         &directly_downloadable=true",
        vendor.name(),
        platform.architecture,
        platform.operating_system,
    );
    if let Some(lib_c) = platform.lib_c {
        url.push_str("&lib_c_type=");
        url.push_str(lib_c);
    }
    url
}

/// A package the API found.
#[derive(Debug, PartialEq, Eq)]
struct Package {
    /// Where its download address and checksum are.
    info_url: String,
    /// `21.0.13.1+2`.
    java_version: String,
    size: u64,
}

/// The first package of the API's answer.
fn package_from(json: &str) -> Result<Package, Failure> {
    let answer: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        Failure::new(FailureKind::Lookup, format!("the package index: {error}"))
    })?;
    let package = answer["result"]
        .as_array()
        .and_then(|packages| packages.first());
    let Some(package) = package else {
        return Err(Failure::new(
            FailureKind::NoPackage,
            "no such package for this OS and architecture".to_owned(),
        ));
    };
    let info_url = package["links"]["pkg_info_uri"].as_str();
    let java_version = package["java_version"].as_str();
    match (info_url, java_version) {
        (Some(info_url), Some(java_version)) => Ok(Package {
            info_url: info_url.to_owned(),
            java_version: java_version.to_owned(),
            size: package["size"].as_u64().unwrap_or(0),
        }),
        _ => Err(Failure::new(
            FailureKind::Lookup,
            "the package index left out the package's address".to_owned(),
        )),
    }
}

/// Where a package downloads from, and how its checksum is known.
#[derive(Debug, PartialEq, Eq)]
struct Archive {
    url: String,
    filename: String,
    checksum: Checksum,
}

#[derive(Debug, PartialEq, Eq)]
enum Checksum {
    /// The SHA-256, as lowercase hex.
    Sha256(String),
    /// A file holding the SHA-256 (`<hex>  <file name>`, or the hex alone).
    Linked(String),
    /// A GitHub release's asset: its SHA-256 is the release's digest of it
    /// (`release` is the API's address of the release).
    GitHubAsset { release: String, name: String },
}

fn archive_from(json: &str) -> Result<Archive, Failure> {
    let answer: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        Failure::new(
            FailureKind::Lookup,
            format!("the package's details: {error}"),
        )
    })?;
    let info = &answer["result"][0];
    let text = |key: &str| info[key].as_str().unwrap_or("").to_owned();
    let url = text("direct_download_uri");
    let filename = text("filename");
    let is_https = url.starts_with("https://");
    let is_plain_name = !filename.is_empty() && !filename.contains(['/', '\\']) && filename != "..";
    if !is_https || !is_plain_name {
        return Err(Failure::new(
            FailureKind::Lookup,
            "the package's details have no HTTPS address".to_owned(),
        ));
    }
    let is_sha256 = text("checksum_type") == "sha256";
    let inline = sha256_hex(&text("checksum"));
    let linked = text("checksum_uri");
    let github_asset = github_release_asset(&url);
    let checksum = match (inline, github_asset) {
        (Some(hex), _) if is_sha256 => Checksum::Sha256(hex),
        _ if linked.starts_with("https://") && is_sha256 => Checksum::Linked(linked),
        (_, Some((release, name))) => Checksum::GitHubAsset { release, name },
        _ => {
            return Err(Failure::new(
                FailureKind::NoChecksum,
                "the package has no SHA-256 checksum to check it against".to_owned(),
            ));
        }
    };
    Ok(Archive {
        url,
        filename,
        checksum,
    })
}

/// A GitHub release asset's download address
/// (`https://github.com/<owner>/<repo>/releases/download/<tag>/<name>`) as
/// the API's address of its release and the asset's name.
fn github_release_asset(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let [owner, repository, "releases", "download", tag, name] = parts[..] else {
        return None;
    };
    let release = format!("https://api.github.com/repos/{owner}/{repository}/releases/tags/{tag}");
    Some((release, name.replace("%2B", "+").replace("%2b", "+")))
}

/// `text`'s first word when it is a SHA-256 in hex, lowercased.
fn sha256_hex(text: &str) -> Option<String> {
    let word = text.split_whitespace().next()?;
    let is_hex = word.len() == 64 && word.chars().all(|char| char.is_ascii_hexdigit());
    is_hex.then(|| word.to_ascii_lowercase())
}

/// Why a download failed: for the log, its kind; for the user, the detail.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) kind: FailureKind,
    /// Shown to the user, never logged: it may hold paths.
    pub(crate) detail: String,
}

impl Failure {
    fn new(kind: FailureKind, detail: String) -> Failure {
        Failure { kind, detail }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// Not macOS or Linux on x64 or aarch64, or no home directory.
    UnsupportedPlatform,
    /// `curl` or `tar` couldn't be run.
    CannotRun,
    /// The package index couldn't be read.
    Lookup,
    NoPackage,
    NoChecksum,
    Download,
    ChecksumMismatch,
    Unpack,
    /// The archive held no JDK.
    NoJdkInArchive,
    /// `~/.jdks` couldn't be written.
    Install,
    /// The download panicked (the log has where).
    Panicked,
}

impl FailureKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            FailureKind::UnsupportedPlatform => "unsupported-platform",
            FailureKind::CannotRun => "cannot-run",
            FailureKind::Lookup => "lookup-failed",
            FailureKind::NoPackage => "no-package",
            FailureKind::NoChecksum => "no-checksum",
            FailureKind::Download => "download-failed",
            FailureKind::ChecksumMismatch => "checksum-mismatch",
            FailureKind::Unpack => "unpack-failed",
            FailureKind::NoJdkInArchive => "no-jdk-in-archive",
            FailureKind::Install => "install-failed",
            FailureKind::Panicked => "panicked",
        }
    }
}

/// A JDK in `~/.jdks`, downloaded now or found there.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InstalledJdk {
    pub(crate) home: PathBuf,
    /// Its directory's name (`temurin-21.0.13.1`).
    pub(crate) name: String,
    pub(crate) already_there: bool,
}

/// The directory a download is made and unpacked in, beside the JDKs.
const STAGING: &str = ".kotlin-analyzer-download";

/// Downloads the latest `release` of `vendor` into `jdks`, reporting
/// what it does (a message, a percentage).
pub(crate) fn install(
    vendor: Vendor,
    release: u32,
    jdks: &Path,
    report: &dyn Fn(String, Option<u32>),
) -> Result<InstalledJdk, Failure> {
    let platform = Platform::current().ok_or_else(|| {
        Failure::new(
            FailureKind::UnsupportedPlatform,
            format!(
                "no JDK download for {} on {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        )
    })?;
    report(format!("Finding {} {release}", vendor.label()), None);
    let found = match vendor {
        Vendor::Corretto => latest_corretto(release, platform)?,
        _ => from_foojay(vendor, release, platform)?,
    };
    let FoundJdk {
        url,
        filename,
        java_version,
        size,
        checksum,
    } = found;
    let name = format!(
        "{}-{}",
        vendor.directory_prefix(),
        version_without_build(&java_version)
    );
    if let Some(home) = jdk_home_in(&jdks.join(&name)) {
        return Ok(InstalledJdk {
            home,
            name,
            already_there: true,
        });
    }
    let staging = Staging::new(jdks, &name)?;
    let downloaded = staging.directory.join(&filename);
    let label = format!("{} {java_version}", vendor.label());
    download(&url, &downloaded, size, &|done| {
        let message = if size == 0 {
            format!("{label}: {} MB", done / 1_000_000)
        } else {
            format!("{label}: {} of {} MB", done / 1_000_000, size / 1_000_000)
        };
        let percentage = super::progress::percentage(done as usize, size as usize);
        report(message, percentage);
    })?;
    report(format!("Checking and unpacking {label}"), None);
    install_archive(&downloaded, &checksum, jdks, &name, &staging)
}

/// A JDK package found: where it downloads from, its version, its size
/// (0 when unknown) and its SHA-256.
struct FoundJdk {
    url: String,
    filename: String,
    java_version: String,
    size: u64,
    checksum: String,
}

/// The latest GA package of `vendor`'s `release`, as the Foojay API has it.
fn from_foojay(vendor: Vendor, release: u32, platform: Platform) -> Result<FoundJdk, Failure> {
    let package = package_from(&fetch(&packages_url(vendor, release, platform))?)?;
    let archive = archive_from(&fetch(&package.info_url)?)?;
    let checksum = match &archive.checksum {
        Checksum::Sha256(hex) => hex.clone(),
        Checksum::Linked(url) => sha256_hex(&fetch(url)?).ok_or_else(|| {
            Failure::new(
                FailureKind::NoChecksum,
                "the package's checksum file holds no SHA-256".to_owned(),
            )
        })?,
        Checksum::GitHubAsset { release, name } => github_asset_sha256(&fetch(release)?, name)
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::NoChecksum,
                    format!("GitHub has no SHA-256 of {name}"),
                )
            })?,
    };
    Ok(FoundJdk {
        url: archive.url,
        filename: archive.filename,
        java_version: package.java_version,
        size: package.size,
        checksum,
    })
}

/// Corretto's latest `release`: the archive Amazon's `latest` address
/// redirects to, and the SHA-256 its `latest_sha256` gives — the Foojay
/// API has no checksum of Corretto's. A release between the two answers
/// leaves them disagreeing, and nothing is installed.
fn latest_corretto(release: u32, platform: Platform) -> Result<FoundJdk, Failure> {
    let file = corretto_file(release, platform);
    let url = redirect_of(&format!("{CORRETTO_DOWNLOADS}/latest/{file}"))?;
    let (filename, java_version) = corretto_archive(&url).ok_or_else(|| {
        Failure::new(
            FailureKind::Lookup,
            format!("Corretto's latest {release} isn't an archive: {url}"),
        )
    })?;
    let checksum = sha256_hex(&fetch(&format!(
        "{CORRETTO_DOWNLOADS}/latest_sha256/{file}"
    ))?)
    .ok_or_else(|| {
        Failure::new(
            FailureKind::NoChecksum,
            "Amazon gives no SHA-256 of Corretto's latest".to_owned(),
        )
    })?;
    Ok(FoundJdk {
        url,
        filename,
        java_version,
        size: 0,
        checksum,
    })
}

const CORRETTO_DOWNLOADS: &str = "https://corretto.aws/downloads";

/// Amazon's name of Corretto's latest archive: `amazon-corretto-21-aarch64-macos-jdk.tar.gz`.
fn corretto_file(release: u32, platform: Platform) -> String {
    format!(
        "amazon-corretto-{release}-{}-{}-jdk.tar.gz",
        platform.architecture, platform.operating_system
    )
}

/// The file name and version of a Corretto download address
/// (`…/downloads/resources/21.0.13.1.2/amazon-corretto-21.0.13.1.2-macosx-aarch64.tar.gz`).
fn corretto_archive(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix(&format!("{CORRETTO_DOWNLOADS}/resources/"))?;
    let (version, filename) = rest.split_once('/')?;
    let is_plain_name = filename.ends_with(".tar.gz") && !filename.contains(['/', '\\']);
    let is_version = !version.is_empty()
        && version
            .chars()
            .all(|char| char.is_ascii_digit() || char == '.');
    if !is_plain_name || !is_version {
        return None;
    }
    Some((filename.to_owned(), version.to_owned()))
}

/// The SHA-256 GitHub's release (its API's JSON) records of its asset `name`.
fn github_asset_sha256(release: &str, name: &str) -> Option<String> {
    let release: serde_json::Value = serde_json::from_str(release).ok()?;
    let asset = release["assets"]
        .as_array()?
        .iter()
        .find(|asset| asset["name"].as_str() == Some(name))?;
    let digest = asset["digest"].as_str()?.strip_prefix("sha256:")?;
    sha256_hex(digest)
}

/// `21.0.13.1+2` without its build: `21.0.13.1`.
fn version_without_build(java_version: &str) -> &str {
    java_version.split('+').next().unwrap_or(java_version)
}

/// Where one download is made and unpacked, removed when done — whether it
/// succeeded or failed.
struct Staging {
    directory: PathBuf,
}

impl Staging {
    fn new(jdks: &Path, name: &str) -> Result<Staging, Failure> {
        let directory = jdks.join(STAGING).join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).map_err(|error| {
            Failure::new(
                FailureKind::Install,
                format!("{}: {error}", directory.display()),
            )
        })?;
        Ok(Staging { directory })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
        if let Some(parent) = self.directory.parent() {
            // Only when no other download is using it.
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Checks `archive` against `checksum`, unpacks it in `staging` and moves
/// the JDK it holds to `jdks/name`.
fn install_archive(
    archive: &Path,
    checksum: &str,
    jdks: &Path,
    name: &str,
    staging: &Staging,
) -> Result<InstalledJdk, Failure> {
    let actual = file_sha256(archive).map_err(|error| {
        Failure::new(
            FailureKind::Download,
            format!("{}: {error}", archive.display()),
        )
    })?;
    if actual != checksum {
        return Err(Failure::new(
            FailureKind::ChecksumMismatch,
            format!("the download's SHA-256 is {actual}, not {checksum}: not installed"),
        ));
    }
    let unpacked = staging.directory.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|error| Failure::new(FailureKind::Install, error.to_string()))?;
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::new(FailureKind::CannotRun, format!("cannot run tar: {error}"))
        })?;
    if !output.status.success() {
        return Err(Failure::new(
            FailureKind::Unpack,
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let top = single_directory(&unpacked)
        .filter(|top| jdk_home_in(top).is_some())
        .ok_or_else(|| {
            Failure::new(
                FailureKind::NoJdkInArchive,
                "the archive holds no JDK".to_owned(),
            )
        })?;
    let target = jdks.join(name);
    std::fs::rename(&top, &target).map_err(|error| {
        Failure::new(
            FailureKind::Install,
            format!("{}: {error}", target.display()),
        )
    })?;
    let home = jdk_home_in(&target).ok_or_else(|| {
        Failure::new(
            FailureKind::NoJdkInArchive,
            "the archive holds no JDK".to_owned(),
        )
    })?;
    Ok(InstalledJdk {
        home,
        name: name.to_owned(),
        already_there: false,
    })
}

/// The one directory `directory` holds, macOS's `._` files aside.
fn single_directory(directory: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with("._"))
        .map(|entry| entry.path())
        .collect();
    match entries.as_slice() {
        [only] if only.is_dir() => Some(only.clone()),
        _ => None,
    }
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// `curl`'s arguments for an HTTPS-only request that fails on an HTTP
/// error and gives up on a stalled transfer.
fn curl() -> Command {
    let mut command = Command::new("curl");
    command
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args([
            "--connect-timeout",
            "30",
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
        ])
        .stdin(Stdio::null());
    command
}

/// Where `url` redirects to (its `Location`), not followed.
fn redirect_of(url: &str) -> Result<String, Failure> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--proto", "=https"])
        .args(["--connect-timeout", "30", "--max-time", "60"])
        .args(["--output", "/dev/null", "--write-out", "%{redirect_url}"])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::new(FailureKind::CannotRun, format!("cannot run curl: {error}"))
        })?;
    let location = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || !location.starts_with("https://") {
        return Err(Failure::new(
            FailureKind::Lookup,
            format!("{url}: {}", String::from_utf8_lossy(&output.stderr).trim()),
        ));
    }
    Ok(location)
}

/// How much of a download of `size` bytes (0: unknown) to report when
/// `done` bytes are on disk: never more than the whole, when it's known.
fn arrived(done: u64, size: u64) -> u64 {
    let size_known = size > 0;
    if size_known { done.min(size) } else { done }
}

/// The body at `url`, as text.
fn fetch(url: &str) -> Result<String, Failure> {
    let output = curl()
        .args(["--max-time", "60"])
        .arg(url)
        .output()
        .map_err(|error| {
            Failure::new(FailureKind::CannotRun, format!("cannot run curl: {error}"))
        })?;
    if !output.status.success() {
        return Err(Failure::new(
            FailureKind::Lookup,
            format!("{url}: {}", String::from_utf8_lossy(&output.stderr).trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Downloads `url` to `destination`, telling `progress` how many bytes
/// have arrived as they do.
fn download(
    url: &str,
    destination: &Path,
    size: u64,
    progress: &dyn Fn(u64),
) -> Result<(), Failure> {
    let mut child = curl()
        .arg("--output")
        .arg(destination)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Failure::new(FailureKind::CannotRun, format!("cannot run curl: {error}"))
        })?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                return Err(Failure::new(FailureKind::Download, error.to_string()));
            }
        }
        let done = std::fs::metadata(destination).map_or(0, |metadata| metadata.len());
        progress(arrived(done, size));
        std::thread::sleep(Duration::from_millis(250));
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(Failure::new(
        FailureKind::Download,
        format!("{url}: {}", stderr.trim()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "kotlin-analyzer-jdk-download-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn the_vendor_question_offers_vendors_with_checksums_then_not_now() {
        let question = JdkQuestion::Vendor {
            project_release: None,
        };
        let titles: Vec<String> = question
            .params()
            .actions
            .unwrap()
            .into_iter()
            .map(|action| action.title)
            .collect();
        assert_eq!(
            titles,
            [
                "Eclipse Temurin",
                "Azul Zulu",
                "Amazon Corretto",
                "BellSoft Liberica",
                "GraalVM Community",
                "Microsoft",
                "Oracle OpenJDK",
                "Not now"
            ]
        );
        assert_eq!(
            question.answer(Some("Azul Zulu")),
            JdkAnswer::Vendor(Vendor::Zulu)
        );
        assert_eq!(question.answer(Some("Not now")), JdkAnswer::Declined);
        assert_eq!(question.answer(None), JdkAnswer::Declined);
    }

    #[test]
    fn the_release_question_puts_the_projects_release_first() {
        let titles = |question: &JdkQuestion| -> Vec<String> {
            question
                .params()
                .actions
                .unwrap()
                .into_iter()
                .map(|action| action.title)
                .collect()
        };
        let none = JdkQuestion::release_of(Vendor::Temurin, None);
        assert_eq!(
            titles(&none),
            ["21 (LTS)", "25 (LTS)", "17 (LTS)", "Cancel"]
        );
        let lts = JdkQuestion::release_of(Vendor::Temurin, Some(17));
        assert_eq!(titles(&lts), ["17 (LTS)", "21 (LTS)", "25 (LTS)", "Cancel"]);
        let other = JdkQuestion::release_of(Vendor::Temurin, Some(11));
        assert_eq!(
            titles(&other),
            [
                "11 (the project's)",
                "21 (LTS)",
                "25 (LTS)",
                "17 (LTS)",
                "Cancel"
            ]
        );
        assert_eq!(
            other.answer(Some("11 (the project's)")),
            JdkAnswer::Release(Vendor::Temurin, 11)
        );
        assert_eq!(
            other.answer(Some("25 (LTS)")),
            JdkAnswer::Release(Vendor::Temurin, 25)
        );
        assert_eq!(other.answer(Some("Cancel")), JdkAnswer::Declined);
    }

    #[test]
    fn the_package_query_names_the_vendor_release_and_platform() {
        let linux = Platform {
            operating_system: "linux",
            architecture: "x64",
            lib_c: Some("glibc"),
        };
        let url = packages_url(Vendor::GraalVmCommunity, 21, linux);
        assert!(url.starts_with(FOOJAY_PACKAGES), "{url}");
        for part in [
            "version=21",
            "distribution=graalvm_community",
            "architecture=x64",
            "operating_system=linux",
            "archive_type=tar.gz",
            "package_type=jdk",
            "lib_c_type=glibc",
        ] {
            assert!(url.contains(part), "{url} lacks {part}");
        }
    }

    #[test]
    fn a_package_and_its_archive_are_read_from_the_index() {
        let packages = r#"{"result":[{"id":"a41b","distribution":"temurin",
            "java_version":"21.0.13.1+2","size":199772678,
            "links":{"pkg_info_uri":"https://api.foojay.io/disco/v3.0/ids/a41b"}}],"message":""}"#;
        assert_eq!(
            package_from(packages).unwrap(),
            Package {
                info_url: "https://api.foojay.io/disco/v3.0/ids/a41b".to_owned(),
                java_version: "21.0.13.1+2".to_owned(),
                size: 199772678,
            }
        );
        let none = package_from(r#"{"result":[],"message":""}"#).unwrap_err();
        assert_eq!(none.kind, FailureKind::NoPackage);

        let hex = "F6422095eb0d21b6d974cb5af85c121728dfe4bbda0108b1420978e793575adc";
        let inline = format!(
            r#"{{"result":[{{"filename":"zulu.tar.gz","direct_download_uri":"https://cdn.azul.com/zulu.tar.gz",
            "checksum_uri":"","checksum":"{hex}","checksum_type":"sha256"}}]}}"#
        );
        assert_eq!(
            archive_from(&inline).unwrap().checksum,
            Checksum::Sha256(hex.to_ascii_lowercase())
        );
        let linked = r#"{"result":[{"filename":"jdk.tar.gz","direct_download_uri":"https://github.com/jdk.tar.gz",
            "checksum_uri":"https://github.com/jdk.tar.gz.sha256.txt","checksum":"","checksum_type":"sha256"}]}"#;
        assert_eq!(
            archive_from(linked).unwrap().checksum,
            Checksum::Linked("https://github.com/jdk.tar.gz.sha256.txt".to_owned())
        );
        let without = r#"{"result":[{"filename":"jdk.tar.gz","direct_download_uri":"https://corretto.aws/jdk.tar.gz",
            "checksum_uri":"","checksum":"","checksum_type":""}]}"#;
        assert_eq!(
            archive_from(without).unwrap_err().kind,
            FailureKind::NoChecksum
        );
        let plain_http = r#"{"result":[{"filename":"jdk.tar.gz","direct_download_uri":"http://example.com/jdk.tar.gz",
            "checksum":"f6422095eb0d21b6d974cb5af85c121728dfe4bbda0108b1420978e793575adc","checksum_type":"sha256"}]}"#;
        assert_eq!(
            archive_from(plain_http).unwrap_err().kind,
            FailureKind::Lookup
        );
        let escaping = r#"{"result":[{"filename":"../jdk.tar.gz","direct_download_uri":"https://example.com/jdk.tar.gz",
            "checksum":"f6422095eb0d21b6d974cb5af85c121728dfe4bbda0108b1420978e793575adc","checksum_type":"sha256"}]}"#;
        assert_eq!(
            archive_from(escaping).unwrap_err().kind,
            FailureKind::Lookup
        );
    }

    #[test]
    fn a_github_release_asset_is_checked_against_githubs_digest() {
        // Liberica: the Foojay API gives a SHA-1 only.
        let liberica = r#"{"result":[{"filename":"bellsoft-jdk21.0.13.1+5-macos-aarch64.tar.gz",
            "direct_download_uri":"https://github.com/bell-sw/Liberica/releases/download/21.0.13.1%2B5/bellsoft-jdk21.0.13.1+5-macos-aarch64.tar.gz",
            "checksum_uri":"","checksum":"e9ef84354eebf7bd5bd1d1f0c7a8a5b7dcd5f1e3","checksum_type":"sha1"}]}"#;
        assert_eq!(
            archive_from(liberica).unwrap().checksum,
            Checksum::GitHubAsset {
                release:
                    "https://api.github.com/repos/bell-sw/Liberica/releases/tags/21.0.13.1%2B5"
                        .to_owned(),
                name: "bellsoft-jdk21.0.13.1+5-macos-aarch64.tar.gz".to_owned(),
            }
        );
        let hex = "ed7803642ffa4df3f74f409437daee4b97e509e1d249fe69696539c295634bb1";
        let release = format!(
            r#"{{"assets":[{{"name":"other.zip","digest":"sha256:{}"}},
                {{"name":"bellsoft-jdk21.0.13.1+5-macos-aarch64.tar.gz","digest":"sha256:{hex}"}}]}}"#,
            "0".repeat(64)
        );
        assert_eq!(
            github_asset_sha256(&release, "bellsoft-jdk21.0.13.1+5-macos-aarch64.tar.gz")
                .as_deref(),
            Some(hex)
        );
        assert_eq!(github_asset_sha256(&release, "missing.tar.gz"), None);
        assert_eq!(
            github_release_asset("https://example.com/a/b/releases/download/t/n"),
            None
        );
    }

    #[test]
    fn correttos_latest_is_read_from_amazons_addresses() {
        let platform = Platform {
            operating_system: "macos",
            architecture: "aarch64",
            lib_c: None,
        };
        assert_eq!(
            corretto_file(21, platform),
            "amazon-corretto-21-aarch64-macos-jdk.tar.gz"
        );
        assert_eq!(
            corretto_archive(
                "https://corretto.aws/downloads/resources/21.0.13.1.2/amazon-corretto-21.0.13.1.2-macosx-aarch64.tar.gz"
            ),
            Some((
                "amazon-corretto-21.0.13.1.2-macosx-aarch64.tar.gz".to_owned(),
                "21.0.13.1.2".to_owned()
            ))
        );
        // Elsewhere, or not an archive: not Corretto's.
        assert_eq!(
            corretto_archive("https://example.com/downloads/resources/21/jdk.tar.gz"),
            None
        );
        assert_eq!(
            corretto_archive("https://corretto.aws/downloads/resources/21.0.13/x/../jdk.tar.gz"),
            None
        );
    }

    #[test]
    fn a_download_of_unknown_size_reports_what_has_arrived() {
        assert_eq!(
            arrived(5_000_000, 0),
            5_000_000,
            "Corretto's size isn't known"
        );
        assert_eq!(arrived(5_000_000, 3_000_000), 3_000_000);
        assert_eq!(arrived(1_000_000, 3_000_000), 1_000_000);
    }

    #[test]
    fn a_checksum_file_gives_its_first_word() {
        let hex = "48067f2b7c958ed104a9d8a5749be7d25220b0a356b4d2d1bcfaab3e16ad0b14";
        assert_eq!(
            sha256_hex(&format!("{hex}  OpenJDK21U.tar.gz\n")).as_deref(),
            Some(hex)
        );
        assert_eq!(sha256_hex(&format!("{hex}\n")).as_deref(), Some(hex));
        assert_eq!(sha256_hex("not found"), None);
    }

    /// A `.tar.gz` of a fake JDK under `top` (`bundle`: as a macOS bundle).
    fn fake_jdk_archive(directory: &Path, top: &str, bundle: bool) -> PathBuf {
        let source = directory.join("source");
        let home = if bundle {
            source.join(top).join("Contents/Home")
        } else {
            source.join(top)
        };
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/java"), "").unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"21.0.5\"\n").unwrap();
        let archive = directory.join("jdk.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg(top)
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    #[test]
    fn a_checked_archive_is_unpacked_into_place_and_nothing_else_is_left() {
        let directory = scratch("install");
        let jdks = directory.join(".jdks");
        let archive = fake_jdk_archive(&directory, "jdk-21.0.5+11", true);
        let checksum = file_sha256(&archive).unwrap();
        let installed = {
            let staging = Staging::new(&jdks, "temurin-21.0.5").unwrap();
            install_archive(&archive, &checksum, &jdks, "temurin-21.0.5", &staging).unwrap()
        };
        assert_eq!(
            installed,
            InstalledJdk {
                home: jdks.join("temurin-21.0.5/Contents/Home"),
                name: "temurin-21.0.5".to_owned(),
                already_there: false,
            }
        );
        let left: Vec<String> = std::fs::read_dir(&jdks)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["temurin-21.0.5"], "the staging directory is gone");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_wrong_checksum_or_an_archive_without_a_jdk_installs_nothing() {
        let directory = scratch("refused");
        let jdks = directory.join(".jdks");
        let archive = fake_jdk_archive(&directory, "jdk-21.0.5+11", false);
        let wrong = "0".repeat(64);
        let mismatch = {
            let staging = Staging::new(&jdks, "zulu-21.0.5").unwrap();
            install_archive(&archive, &wrong, &jdks, "zulu-21.0.5", &staging).unwrap_err()
        };
        assert_eq!(mismatch.kind, FailureKind::ChecksumMismatch);

        let empty_source = directory.join("empty/nothing");
        std::fs::create_dir_all(&empty_source).unwrap();
        std::fs::write(empty_source.join("README"), "no JDK here").unwrap();
        let not_a_jdk = directory.join("not-a-jdk.tar.gz");
        Command::new("tar")
            .arg("-czf")
            .arg(&not_a_jdk)
            .arg("-C")
            .arg(directory.join("empty"))
            .arg("nothing")
            .status()
            .unwrap();
        let checksum = file_sha256(&not_a_jdk).unwrap();
        let no_jdk = {
            let staging = Staging::new(&jdks, "zulu-21.0.5").unwrap();
            install_archive(&not_a_jdk, &checksum, &jdks, "zulu-21.0.5", &staging).unwrap_err()
        };
        assert_eq!(no_jdk.kind, FailureKind::NoJdkInArchive);
        let left = std::fs::read_dir(&jdks).unwrap().count();
        assert_eq!(left, 0, "nothing installed, nothing staged");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
