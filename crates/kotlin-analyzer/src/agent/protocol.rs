//! What an agent's MCP process and the host analysing a project say to
//! each other over the host's socket: one JSON object per line, a request
//! `{"id": 1, "query": {…}}` answered by `{"id": 1, "answer": "…"}` or
//! `{"id": 1, "error": "…"}`.
//!
//! A host is a language server, run by an editor or by the MCP process
//! itself; it listens on `<sockets>/<hash of its root>.sock`, the directory
//! readable by its owner only. Queries only read.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What an agent asks about the project.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum AgentQuery {
    /// Declarations whose names match `query`.
    Symbols { query: String },
    /// Where what's named at `at` is declared.
    Definition { at: SourcePosition },
    /// Where it is used.
    References { at: SourcePosition },
    /// Its signature, type and documentation.
    Hover { at: SourcePosition },
    /// The subclasses or overrides of a class or member.
    Implementations { at: SourcePosition },
    /// The file's declarations as a tree.
    Outline { file: PathBuf },
    /// What the checks find in the file.
    Diagnostics { file: PathBuf },
}

impl AgentQuery {
    /// The file the query is about, if it names one.
    pub(crate) fn file(&self) -> Option<&Path> {
        match self {
            AgentQuery::Symbols { .. } => None,
            AgentQuery::Definition { at }
            | AgentQuery::References { at }
            | AgentQuery::Hover { at }
            | AgentQuery::Implementations { at } => Some(&at.file),
            AgentQuery::Outline { file } | AgentQuery::Diagnostics { file } => Some(file),
        }
    }

    /// For the log: which query, in this program's words.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            AgentQuery::Symbols { .. } => "symbols",
            AgentQuery::Definition { .. } => "definition",
            AgentQuery::References { .. } => "references",
            AgentQuery::Hover { .. } => "hover",
            AgentQuery::Implementations { .. } => "implementations",
            AgentQuery::Outline { .. } => "outline",
            AgentQuery::Diagnostics { .. } => "diagnostics",
        }
    }
}

/// A place in a file as an agent names it: a line (from 1) and the name on
/// it — or, without a name, a column (from 1, in characters).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(crate) struct SourcePosition {
    pub(crate) file: PathBuf,
    pub(crate) line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) column: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentRequest {
    pub(crate) id: u64,
    pub(crate) query: AgentQuery,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentReply {
    pub(crate) id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl AgentReply {
    pub(crate) fn of(id: u64, answered: Result<String, String>) -> AgentReply {
        match answered {
            Ok(answer) => AgentReply {
                id,
                answer: Some(answer),
                error: None,
            },
            Err(error) => AgentReply {
                id,
                answer: None,
                error: Some(error),
            },
        }
    }
}

/// Writes `message` as one line of JSON.
pub(crate) fn write_line(writer: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one line of JSON; `None` at the end of the stream.
pub(crate) fn read_line<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// The longest path a Unix socket may have (macOS allows 104 bytes with
/// its terminating zero, Linux 108).
pub(crate) const SOCKET_PATH_LIMIT: usize = 103;

/// Where hosts listen: `<cache>/kotlin-analyzer/agent`.
pub(crate) fn default_socket_directory() -> Option<PathBuf> {
    crate::server::cache_directory().map(|cache| cache.join("agent"))
}

/// The socket of the host for `root` (canonical, so a root reached
/// through a symlink is the same root). Its name is a stable hash of the
/// path: the same in every build of the server.
pub(crate) fn socket_path(directory: &Path, root: &Path) -> PathBuf {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    directory.join(format!(
        "{:016x}.sock",
        fnv1a(root.to_string_lossy().as_bytes())
    ))
}

/// FNV-1a, 64-bit: a hash whose value no Rust release changes.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The project a file belongs to: the nearest directory up from it with a
/// Gradle settings script — a git worktree of the project is a project of
/// its own — else the nearest with a build script or `.git`. `None`
/// outside any: no host is started for a directory that isn't a project
/// (a home directory, `/`).
pub(crate) fn project_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let has =
        |directory: &Path, names: &[&str]| names.iter().any(|name| directory.join(name).exists());
    let ancestors: Vec<&Path> = start.ancestors().collect();
    let with_settings = ancestors
        .iter()
        .find(|directory| has(directory, &["settings.gradle.kts", "settings.gradle"]));
    let with_build_or_git = || {
        ancestors
            .iter()
            .find(|directory| has(directory, &["build.gradle.kts", "build.gradle", ".git"]))
    };
    with_settings
        .or_else(with_build_or_git)
        .map(|directory| directory.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_round_trips_as_json_and_names_its_file() {
        let query = AgentQuery::Definition {
            at: SourcePosition {
                file: PathBuf::from("/work/app/Order.kt"),
                line: 12,
                symbol: Some("total".to_owned()),
                column: None,
            },
        };
        let json = serde_json::to_string(&query).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"definition","at":{"file":"/work/app/Order.kt","line":12,"symbol":"total"}}"#
        );
        assert_eq!(serde_json::from_str::<AgentQuery>(&json).unwrap(), query);
        assert_eq!(query.file(), Some(Path::new("/work/app/Order.kt")));
    }

    #[test]
    fn a_worktree_inside_the_project_is_a_project_of_its_own() {
        let root =
            std::env::temp_dir().join(format!("kotlin-analyzer-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let worktree = root.join(".claude/worktrees/fix");
        for directory in [root.join("app/src"), worktree.join("app/src")] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(root.join("settings.gradle.kts"), "").unwrap();
        std::fs::write(worktree.join("settings.gradle.kts"), "").unwrap();
        std::fs::write(root.join("app/build.gradle.kts"), "").unwrap();
        assert_eq!(
            project_root(&root.join("app/src/Order.kt")),
            Some(root.clone())
        );
        assert_eq!(
            project_root(&worktree.join("app/src/Order.kt")),
            Some(worktree.clone())
        );
        let outside =
            std::env::temp_dir().join(format!("kotlin-analyzer-loose-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(project_root(&outside.join("Loose.kt")), None, "no project");
        std::fs::remove_dir_all(&outside).unwrap();
        let sockets = Path::new("/sockets");
        assert_ne!(socket_path(sockets, &root), socket_path(sockets, &worktree));
        assert_eq!(
            socket_path(sockets, &root),
            socket_path(sockets, &root.join("."))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
