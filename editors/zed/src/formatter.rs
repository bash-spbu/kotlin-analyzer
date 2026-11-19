//! The formatters the server runs (`formatting.formatter`): ktrs, the
//! default, and ktfmt. Each is downloaded at a pinned version whose
//! SHA-256 is written here, and taken out of its archive only once that
//! agrees. Plain Rust, so it's tested on the host as well.

use std::io::Read;

use sha2::{Digest, Sha256};

use crate::release::{Machine, System};

pub const KTRS_VERSION: &str = "0.5.3";
pub const KTFMT_VERSION: &str = "0.65";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Formatter {
    Ktrs,
    Ktfmt,
}

impl Formatter {
    /// The setting's formatter: `ktrs` unless it names `ktfmt`; `None` for
    /// `off`.
    pub fn from_setting(setting: Option<&str>) -> Option<Formatter> {
        match setting {
            Some("off") => None,
            Some("ktfmt") => Some(Formatter::Ktfmt),
            _ => Some(Formatter::Ktrs),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Formatter::Ktrs => "ktrs",
            Formatter::Ktfmt => "ktfmt",
        }
    }
}

/// How a download holds the program.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Packaging {
    /// A `.tar.gz` with the executable at `member`.
    TarGz { member: String },
    /// The program itself: ktfmt's jar, run by `java`.
    Jar,
}

/// One formatter's download for one machine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FormatterAsset {
    pub formatter: Formatter,
    pub url: String,
    pub file_name: String,
    pub sha256: &'static str,
    pub packaging: Packaging,
}

impl FormatterAsset {
    pub fn for_machine(formatter: Formatter, system: System, machine: Machine) -> FormatterAsset {
        match formatter {
            Formatter::Ktrs => ktrs(system, machine),
            Formatter::Ktfmt => ktfmt(system, machine),
        }
    }

    /// The directory, relative to the extension's work directory, holding
    /// this version.
    pub fn directory(&self) -> String {
        let version = match self.formatter {
            Formatter::Ktrs => KTRS_VERSION,
            Formatter::Ktfmt => KTFMT_VERSION,
        };
        format!("{}-{version}", self.formatter.name())
    }

    /// The program the server runs: the executable or the jar.
    pub fn program(&self) -> String {
        let file = match self.packaging {
            Packaging::TarGz { .. } => self.formatter.name(),
            Packaging::Jar => self.file_name.as_str(),
        };
        format!("{}/{file}", self.directory())
    }

    /// The program out of `downloaded`, if its SHA-256 is the pinned one.
    pub fn verified_program(&self, downloaded: &[u8]) -> Result<Vec<u8>, String> {
        let actual = Sha256::digest(downloaded)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != self.sha256 {
            return Err(format!(
                "{} has SHA-256 {actual}, expected {}",
                self.file_name, self.sha256
            ));
        }
        match &self.packaging {
            Packaging::Jar => Ok(downloaded.to_vec()),
            Packaging::TarGz { member } => tar_member(downloaded, member)
                .map_err(|error| format!("unpacking {}: {error}", self.file_name)),
        }
    }
}

fn ktrs(system: System, machine: Machine) -> FormatterAsset {
    let (triple, sha256) = match (system, machine) {
        (System::Mac, Machine::Aarch64) => (
            "aarch64-apple-darwin",
            "5dd74fe28b4d14470ec3408675681300146507e701271c37f31152066d7fdb4c",
        ),
        (System::Mac, Machine::X86_64) => (
            "x86_64-apple-darwin",
            "59c5931f05893686b9f4b7c63313a5ec78d4c16e6e3fd2204a248ceb31017bd8",
        ),
        (System::Linux, Machine::Aarch64) => (
            "aarch64-unknown-linux-musl",
            "2e52c6c85a1800f4ca20651abd7e9ff2d67ebe7041dd5b2355028af629ef745d",
        ),
        (System::Linux, Machine::X86_64) => (
            "x86_64-unknown-linux-musl",
            "80913a1e65f8821797d2c7fca3b1f82ca8d1b8c59cbc2e00e8024691608afe62",
        ),
    };
    let stem = format!("ktrs-v{KTRS_VERSION}-{triple}");
    let file_name = format!("{stem}.tar.gz");
    FormatterAsset {
        formatter: Formatter::Ktrs,
        url: format!("https://github.com/Hexay/ktrs/releases/download/v{KTRS_VERSION}/{file_name}"),
        file_name,
        sha256,
        packaging: Packaging::TarGz {
            member: format!("{stem}/ktrs"),
        },
    }
}

/// ktfmt's native build where there is one; its jar (which needs Java)
/// elsewhere.
fn ktfmt(system: System, machine: Machine) -> FormatterAsset {
    let native = match (system, machine) {
        (System::Mac, Machine::Aarch64) => Some((
            "macos-aarch64",
            "d10ec46dadc4ae19e5b8ddc72d10ce45e5be35d632ce9e73c9b8a59d2147fb85",
        )),
        (System::Linux, Machine::X86_64) => Some((
            "linux-x86_64",
            "3684327b092db9b76fffcb9525decf73f8331ed17e00ce3232769c11cd36d352",
        )),
        (System::Mac, Machine::X86_64) | (System::Linux, Machine::Aarch64) => None,
    };
    let release = format!("https://github.com/facebook/ktfmt/releases/download/v{KTFMT_VERSION}");
    match native {
        Some((platform, sha256)) => {
            let stem = format!("ktfmt-{platform}-{KTFMT_VERSION}");
            let file_name = format!("{stem}.tar.gz");
            FormatterAsset {
                formatter: Formatter::Ktfmt,
                url: format!("{release}/{file_name}"),
                file_name,
                sha256,
                packaging: Packaging::TarGz {
                    member: format!("{stem}/ktfmt"),
                },
            }
        }
        None => {
            let file_name = format!("ktfmt-{KTFMT_VERSION}-with-dependencies.jar");
            FormatterAsset {
                formatter: Formatter::Ktfmt,
                url: format!("{release}/{file_name}"),
                file_name,
                sha256: "aba9753b6ab387926179ab7318ba8e0c33dad483ebc72f633e79522265b15214",
                packaging: Packaging::Jar,
            }
        }
    }
}

/// The file at `member` in the gzipped tar `packed`.
fn tar_member(packed: &[u8], member: &str) -> Result<Vec<u8>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(packed));
    let entries = archive.entries().map_err(|error| error.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path().map_err(|error| error.to_string())?;
        if path.to_string_lossy() == member {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            return Ok(bytes);
        }
    }
    Err(format!("no {member} in it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn with_sha256_of(asset: FormatterAsset, bytes: &[u8]) -> FormatterAsset {
        let sha256: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        FormatterAsset {
            sha256: Box::leak(sha256.into_boxed_str()),
            ..asset
        }
    }

    #[test]
    fn ktrs_is_its_release_archive_for_every_machine() {
        let asset = FormatterAsset::for_machine(Formatter::Ktrs, System::Linux, Machine::Aarch64);
        assert_eq!(
            asset.url,
            "https://github.com/Hexay/ktrs/releases/download/v0.5.3/ktrs-v0.5.3-aarch64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset.packaging,
            Packaging::TarGz {
                member: "ktrs-v0.5.3-aarch64-unknown-linux-musl/ktrs".to_owned()
            }
        );
        assert_eq!(asset.program(), "ktrs-0.5.3/ktrs");
    }

    #[test]
    fn ktfmt_is_native_where_it_is_built_and_its_jar_elsewhere() {
        let native = FormatterAsset::for_machine(Formatter::Ktfmt, System::Mac, Machine::Aarch64);
        assert_eq!(
            native.url,
            "https://github.com/facebook/ktfmt/releases/download/v0.65/ktfmt-macos-aarch64-0.65.tar.gz"
        );
        assert_eq!(native.program(), "ktfmt-0.65/ktfmt");
        let jar = FormatterAsset::for_machine(Formatter::Ktfmt, System::Mac, Machine::X86_64);
        assert_eq!(jar.packaging, Packaging::Jar);
        assert_eq!(jar.program(), "ktfmt-0.65/ktfmt-0.65-with-dependencies.jar");
    }

    #[test]
    fn the_program_is_taken_out_only_when_the_download_has_the_pinned_checksum() {
        let program: &[u8] = b"#!/bin/sh\ncat\n";
        let packed = tar_gz(&[
            ("ktrs-v0.5.3-x86_64-apple-darwin/README.md", b"readme"),
            ("ktrs-v0.5.3-x86_64-apple-darwin/ktrs", program),
            ("ktrs-v0.5.3-x86_64-apple-darwin/ktfmt", b"a drop-in"),
        ]);
        let pinned = FormatterAsset::for_machine(Formatter::Ktrs, System::Mac, Machine::X86_64);
        let refused = pinned.verified_program(&packed).unwrap_err();
        assert!(refused.contains("SHA-256"), "{refused}");

        let matching = with_sha256_of(pinned, &packed);
        assert_eq!(matching.verified_program(&packed), Ok(program.to_vec()));

        let without_program = tar_gz(&[("ktrs-v0.5.3-x86_64-apple-darwin/README.md", b"readme")]);
        let missing = with_sha256_of(matching, &without_program)
            .verified_program(&without_program)
            .unwrap_err();
        assert!(missing.contains("no ktrs-v0.5.3"), "{missing}");
    }

    #[test]
    fn the_setting_picks_ktrs_unless_it_says_ktfmt_or_off() {
        assert_eq!(Formatter::from_setting(None), Some(Formatter::Ktrs));
        assert_eq!(
            Formatter::from_setting(Some("ktfmt")),
            Some(Formatter::Ktfmt)
        );
        assert_eq!(
            Formatter::from_setting(Some("prettier")),
            Some(Formatter::Ktrs)
        );
        assert_eq!(Formatter::from_setting(Some("off")), None);
    }
}
