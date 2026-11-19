//! The key log hashes are made with: random, made once per machine and
//! kept in the cache directory (`log-key`) — never written to a log, so
//! only this machine's user can tell what a hash stands for
//! (`kotlin-analyzer log-whois`).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

pub(crate) fn current() -> (u64, u64) {
    static KEY: OnceLock<(u64, u64)> = OnceLock::new();
    *KEY.get_or_init(|| stored().unwrap_or_else(random))
}

/// The machine's key, made and stored on first use.
fn stored() -> Option<(u64, u64)> {
    let path = crate::server::cache_directory()?.join("log-key");
    if let Some(key) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| parse(&text))
    {
        return Some(key);
    }
    let key = random();
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, format!("{:016x} {:016x}\n", key.0, key.1)).ok()?;
    Some(key)
}

fn parse(text: &str) -> Option<(u64, u64)> {
    let mut parts = text.split_whitespace();
    let first = u64::from_str_radix(parts.next()?, 16).ok()?;
    let second = u64::from_str_radix(parts.next()?, 16).ok()?;
    Some((first, second))
}

/// Two words from the operating system's randomness (the standard
/// library seeds each `RandomState` from it).
fn random() -> (u64, u64) {
    let word = |salt: u64| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.finish()
    };
    (word(1), word(2))
}
