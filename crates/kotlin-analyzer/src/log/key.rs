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

/// The machine's key, made and stored on first use — readable by its user
/// only. Tests have a fixed one and leave the user's cache alone.
fn stored() -> Option<(u64, u64)> {
    if cfg!(test) {
        return Some((1, 2));
    }
    let path = crate::server::cache_directory()?.join("log-key");
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| parse(&text));
    if let Some(key) = existing {
        make_private(&path);
        return Some(key);
    }
    let key = random();
    std::fs::create_dir_all(path.parent()?).ok()?;
    write_private(&path, &format!("{:016x} {:016x}\n", key.0, key.1))?;
    Some(key)
}

/// Writes a file only its owner may read.
fn write_private(path: &std::path::Path, text: &str) -> Option<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).ok()?;
    file.write_all(text.as_bytes()).ok()?;
    make_private(path);
    Some(())
}

/// A key stored before it was kept private (or under a wider umask) is
/// made private.
fn make_private(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
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

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn the_key_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("kotlin-analyzer-key-{}", std::process::id()));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::write_private(&path, "1 2\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_file(&path).unwrap();
    }
}
