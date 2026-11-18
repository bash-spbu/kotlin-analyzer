//! `kotlin-analyzer diagnostics <root>`: every check over every workspace
//! file, as the corpus gate runs it (`docs/spec/diagnostics.md`).

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ide::{AnalysisHost, Check, DiagnosticsConfig, Severity};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use vfs::Vfs;

use crate::analysis_stats::resident_memory;
use crate::load;

/// How many files are checked between evictions of cached results.
const BATCH: usize = 128;

struct Options<'arguments> {
    root: &'arguments Path,
    project_json: Option<&'arguments Path>,
    config: DiagnosticsConfig,
    /// How many findings are printed (all are counted).
    limit: usize,
}

pub fn command(arguments: &[String]) -> ExitCode {
    let options = match parse(arguments) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let loaded = match load::read_workspace(options.root, options.project_json) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("kotlin-analyzer: {error}");
            return ExitCode::FAILURE;
        }
    };
    let model = loaded.model.clone();
    let mut host = AnalysisHost::default();
    let mut vfs = Vfs::default();
    load::apply_files(&mut host, &mut vfs, &model, loaded.files, |_| false);
    load::apply_libraries(&mut host, loaded.libraries);
    let files: Vec<vfs::FileId> = vfs.existing_files().collect();

    let started = Instant::now();
    let mut counts: FxHashMap<Check, usize> = FxHashMap::default();
    let mut durations: Vec<(vfs::FileId, Duration)> = Vec::with_capacity(files.len());
    let mut printed = 0;
    let mut failing = false;
    let mut peak_memory = 0;
    for batch in files.chunks(BATCH) {
        let chunk = batch
            .len()
            .div_ceil(rayon::current_num_threads().max(1))
            .max(1);
        let work: Vec<(ide::Analysis, &[vfs::FileId])> = batch
            .chunks(chunk)
            .map(|part| (host.analysis(), part))
            .collect();
        let results: Vec<(vfs::FileId, Duration, Vec<ide::Diagnostic>)> = work
            .into_par_iter()
            .flat_map_iter(|(analysis, part)| {
                part.iter()
                    .filter_map(|file| {
                        let file_started = Instant::now();
                        let found = analysis.diagnostics(*file, &options.config).ok()?;
                        Some((*file, file_started.elapsed(), found))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for (file, duration, found) in results {
            durations.push((file, duration));
            for diagnostic in found {
                *counts.entry(diagnostic.check).or_default() += 1;
                let severity = diagnostic.severity;
                failing |= matches!(severity, Severity::Error | Severity::Warning);
                if printed < options.limit {
                    printed += 1;
                    let text = vfs.contents(file).cloned().unwrap_or_default();
                    let position = vfs::LineIndex::new(&text)
                        .line_column(&text, diagnostic.range.start as usize);
                    println!(
                        "{}:{}:{} [{}] {}",
                        vfs.path(file).display(),
                        position.line + 1,
                        position.column + 1,
                        diagnostic.check.name(),
                        diagnostic.message
                    );
                }
            }
        }
        peak_memory = peak_memory.max(resident_memory().unwrap_or(0));
        // Every snapshot is dropped by now.
        host.trigger_lru_eviction();
        if std::env::var_os("KOTLIN_ANALYZER_TRACE_MEMORY").is_some() {
            eprintln!(
                "after batch: {:.0} MB",
                resident_memory().unwrap_or(0) as f64 / 1_048_576.0
            );
        }
    }

    println!("files           : {}", files.len());
    println!("time            : {:?}", started.elapsed());
    let mut sorted: Vec<Duration> = durations.iter().map(|(_, duration)| *duration).collect();
    sorted.sort();
    if let Some(slowest) = durations.iter().max_by_key(|(_, duration)| *duration) {
        let percentile = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction) as usize];
        println!(
            "per file        : p50 {:?}, p95 {:?}, max {:?} ({})",
            percentile(0.5),
            percentile(0.95),
            slowest.1,
            vfs.path(slowest.0).display()
        );
    }
    println!(
        "peak memory     : {:.0} MB (RSS)",
        peak_memory as f64 / 1_048_576.0
    );
    for check in Check::ALL {
        if options.config.runs(check) {
            println!(
                "{:<24}: {}",
                check.name(),
                counts.get(&check).copied().unwrap_or(0)
            );
        }
    }
    if failing {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn parse(arguments: &[String]) -> Result<Options<'_>, String> {
    const USAGE: &str = "usage: kotlin-analyzer diagnostics <root> [--project-json <file>] [--only <check,…>] [--experimental] [--limit <n>]";
    let mut root = None;
    let mut project_json = None;
    let mut config = DiagnosticsConfig::default();
    let mut limit = 200;
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--project-json" => project_json = remaining.next().map(Path::new),
            "--experimental" => config.experimental = true,
            "--limit" => {
                limit = remaining
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or(USAGE)?;
            }
            "--only" => {
                let names = remaining.next().ok_or(USAGE)?;
                for name in names.split(',') {
                    let check =
                        Check::from_name(name).ok_or_else(|| format!("unknown check `{name}`"))?;
                    config.only.push(check);
                }
            }
            other => root = Some(Path::new(other)),
        }
    }
    Ok(Options {
        root: root.ok_or(USAGE)?,
        project_json,
        config,
        limit,
    })
}
