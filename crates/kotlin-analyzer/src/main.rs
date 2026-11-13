//! `kotlin-analyzer`: the language server (LSP over stdio) and its command
//! line tools for measuring it on a real workspace.

mod analysis_stats;
mod diagnostics;
mod load;
mod server;

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
kotlin-analyzer — a Kotlin language server

USAGE:
    kotlin-analyzer             run the language server on stdin/stdout
    kotlin-analyzer analysis-stats <root> [--project-json <file>]
                                load a workspace and report sizes, times, memory
                                and how many type names resolve
    kotlin-analyzer diagnostics <root> [--project-json <file>] [--only <check,…>]
                                [--experimental] [--limit <n>]
                                run every check over every workspace file; exits 1
                                on any error or warning
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
        Some("analysis-stats") => analysis_stats_command(&arguments[1..]),
        Some("diagnostics") => diagnostics::command(&arguments[1..]),
        Some("definition") => analysis_stats::definition_command(&arguments[1..]),
        Some("inference") => analysis_stats::inference_command(&arguments[1..]),
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => match server::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("kotlin-analyzer: {error}");
                ExitCode::FAILURE
            }
        },
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

fn analysis_stats_command(arguments: &[String]) -> ExitCode {
    let mut root = None;
    let mut project_json = None;
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--project-json" => project_json = remaining.next().map(Path::new),
            other => root = Some(Path::new(other)),
        }
    }
    let Some(root) = root else {
        eprintln!("usage: kotlin-analyzer analysis-stats <root> [--project-json <file>]");
        return ExitCode::FAILURE;
    };
    analysis_stats::run(root, project_json)
}
