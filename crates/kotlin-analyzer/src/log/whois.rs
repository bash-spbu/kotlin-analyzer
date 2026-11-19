//! `kotlin-analyzer log-whois <root> <hash>…`: which of the workspace's
//! files (or the workspace itself) a log's hash stands for — told only on
//! the machine whose key made it, to its user.

use std::path::Path;
use std::process::ExitCode;

use walkdir::WalkDir;

use super::fields::Hashed;

pub(crate) fn command(arguments: &[String]) -> ExitCode {
    let Some((root, hashes)) = arguments.split_first() else {
        eprintln!("usage: kotlin-analyzer log-whois <root> <hash>…");
        return ExitCode::FAILURE;
    };
    let root = Path::new(root);
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let wanted: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let mut found = 0;
    let workspace = Hashed::of(&root.to_string_lossy()).to_string();
    if wanted.contains(&workspace.as_str()) {
        println!("{workspace}  (the workspace) {}", root.display());
        found += 1;
    }
    let files = WalkDir::new(&root).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_str().unwrap_or("");
        entry.depth() == 0 || !project_model::is_excluded_directory(name)
    });
    for entry in files.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let hash = Hashed::path(entry.path(), &root).to_string();
        if wanted.contains(&hash.as_str()) {
            println!("{hash}  {}", entry.path().display());
            found += 1;
        }
    }
    if found == 0 {
        eprintln!(
            "kotlin-analyzer: no file under {} has these hashes (on this machine's key)",
            root.display()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
