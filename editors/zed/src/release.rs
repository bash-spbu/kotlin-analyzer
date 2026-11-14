//! The server's GitHub releases: which asset fits this machine, where it is,
//! and unpacking it once its checksum agrees. Plain Rust, so it's tested on
//! the host as well as run in Zed's WebAssembly.

use std::io::Read;

use sha2::{Digest, Sha256};

/// The server release this extension downloads; `cargo xtask versions`
/// keeps it the server's own version.
pub const SERVER_VERSION: &str = "1.0.0";

/// Where releases are downloaded from: `<base>/v<version>/<asset>`. A build
/// with `KOTLIN_ANALYZER_DOWNLOADS` set points elsewhere (the Zed test does).
const DOWNLOADS: &str = match option_env!("KOTLIN_ANALYZER_DOWNLOADS") {
    Some(base) => base,
    None => "https://github.com/bash-spbu/kotlin-analyzer/releases/download",
};

/// The machines the release workflow builds for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum System {
    Mac,
    Linux,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Machine {
    Aarch64,
    X86_64,
}

/// One release's binary for one machine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asset {
    pub version: &'static str,
    pub system: System,
    pub machine: Machine,
}

impl Asset {
    pub fn for_machine(system: System, machine: Machine) -> Asset {
        Asset {
            version: SERVER_VERSION,
            system,
            machine,
        }
    }

    pub fn triple(&self) -> String {
        let machine = match self.machine {
            Machine::Aarch64 => "aarch64",
            Machine::X86_64 => "x86_64",
        };
        let system = match self.system {
            System::Mac => "apple-darwin",
            System::Linux => "unknown-linux-gnu",
        };
        format!("{machine}-{system}")
    }

    pub fn file_name(&self) -> String {
        format!("kotlin-analyzer-{}.gz", self.triple())
    }

    pub fn url(&self) -> String {
        format!("{DOWNLOADS}/v{}/{}", self.version, self.file_name())
    }

    pub fn checksum_url(&self) -> String {
        format!("{}.sha256", self.url())
    }

    /// The directory, relative to the extension's work directory, holding
    /// this version's binary.
    pub fn directory(&self) -> String {
        format!("kotlin-analyzer-{}", self.version)
    }

    pub fn binary(&self) -> String {
        format!("{}/kotlin-analyzer", self.directory())
    }
}

/// The binary inside `packed` (the gzipped asset), if its SHA-256 is the one
/// the release's `.sha256` file (`checksum_file`) states.
pub fn verified_binary(packed: &[u8], checksum_file: &str, asset: &str) -> Result<Vec<u8>, String> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual = Sha256::digest(packed)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if expected != actual {
        return Err(format!(
            "{asset} has SHA-256 {actual}, the release says {expected}"
        ));
    }
    let mut binary = Vec::new();
    flate2::read::GzDecoder::new(packed)
        .read_to_end(&mut binary)
        .map_err(|error| format!("unpacking {asset}: {error}"))?;
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn gzipped(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn an_asset_names_the_tag_the_triple_and_its_directory() {
        let asset = Asset::for_machine(System::Mac, Machine::Aarch64);
        assert_eq!(
            asset.url(),
            format!(
                "https://github.com/bash-spbu/kotlin-analyzer/releases/download/v{SERVER_VERSION}/kotlin-analyzer-aarch64-apple-darwin.gz"
            )
        );
        assert_eq!(asset.checksum_url(), format!("{}.sha256", asset.url()));
        assert_eq!(
            asset.binary(),
            format!("kotlin-analyzer-{SERVER_VERSION}/kotlin-analyzer")
        );
        let linux = Asset::for_machine(System::Linux, Machine::X86_64);
        assert_eq!(linux.triple(), "x86_64-unknown-linux-gnu");
    }

    #[test]
    fn a_binary_is_unpacked_only_when_its_checksum_agrees() {
        let binary = b"#!/bin/sh\necho kotlin-analyzer\n";
        let packed = gzipped(binary);
        let checksum_file = format!("{}  asset.gz\n", sha256_hex(&packed).to_uppercase());
        assert_eq!(
            verified_binary(&packed, &checksum_file, "asset.gz"),
            Ok(binary.to_vec())
        );

        let wrong = format!("{}  asset.gz\n", "0".repeat(64));
        let refused = verified_binary(&packed, &wrong, "asset.gz").unwrap_err();
        assert!(refused.contains("SHA-256"), "{refused}");
        assert!(verified_binary(&packed, "", "asset.gz").is_err());
    }
}
