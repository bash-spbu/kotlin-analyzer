//! `kotlin-analyzer`: the language server (LSP over stdio) and its command
//! line tools for measuring it on a real workspace.

use std::process::ExitCode;

const USAGE: &str = "\
kotlin-analyzer — a Kotlin language server

USAGE:
    kotlin-analyzer             run the language server on stdin/stdout
    kotlin-analyzer parse <file>
                                print a Kotlin or Java file's syntax tree
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
        Some("parse") => parse_command(arguments.get(1)),
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

fn parse_command(path: Option<&String>) -> ExitCode {
    let Some(path) = path else {
        eprintln!("usage: kotlin-analyzer parse <file>");
        return ExitCode::FAILURE;
    };
    let path = std::path::Path::new(path);
    let Some(language) = syntax::Language::of_path(path) else {
        eprintln!(
            "kotlin-analyzer: {} is not a Kotlin or Java file",
            path.display()
        );
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("kotlin-analyzer: cannot read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let parse = syntax::parse(text.into(), language);
    print!("{}", parse.debug_tree());
    for error in parse.errors() {
        println!(
            "error at {}..{}: {}",
            error.range.start, error.range.end, error.message
        );
    }
    ExitCode::SUCCESS
}
