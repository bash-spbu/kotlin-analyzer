//! `kotlin-analyzer`: the language server (LSP over stdio) and its command
//! line tools for measuring it on a real workspace.

use std::process::ExitCode;

const USAGE: &str = "\
kotlin-analyzer — a Kotlin language server

USAGE:
    kotlin-analyzer             run the language server on stdin/stdout
    kotlin-analyzer --version   print the version
    kotlin-analyzer --help      print this help
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("kotlin-analyzer {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("kotlin-analyzer: the language server isn't implemented yet");
            ExitCode::FAILURE
        }
        Some(unknown) => {
            eprintln!("kotlin-analyzer: unknown argument `{unknown}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
