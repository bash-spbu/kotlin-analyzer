//! `kotlin-analyzer analysis-stats <root>`: loads a workspace like the server
//! does and reports its size, time per phase, memory, and how many written
//! type names resolve.

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use ide::AnalysisHost;
use rayon::prelude::*;
use vfs::Vfs;

use crate::load;

pub fn run(root: &Path, project_json: Option<&Path>) -> ExitCode {
    let total = Instant::now();
    let loaded = match load::read_workspace(root, project_json) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("kotlin-analyzer: {error}");
            return ExitCode::FAILURE;
        }
    };
    let model = loaded.model.clone();
    println!("workspace       : {}", root.display());
    println!("model           : {:?}", model.origin);
    println!("modules         : {}", model.modules.len());
    println!("source sets     : {}", model.source_sets.len());
    println!(
        "libraries       : {} (distinct jars)",
        distinct_libraries(&model)
    );
    println!("files           : {}", loaded.files.len());
    let bytes: usize = loaded.files.iter().map(|(_, text)| text.len()).sum();
    println!("source size     : {:.1} MB", bytes as f64 / 1_048_576.0);
    println!("time: model     : {:?}", loaded.model_time);
    println!("time: discovery : {:?}", loaded.discovery_time);
    println!("time: reading   : {:?}", loaded.read_time);
    let indexed_classes: usize = loaded
        .libraries
        .libraries
        .iter()
        .flat_map(|(_, indexed)| indexed.packages.iter())
        .map(|package| package.names.len())
        .sum();
    println!(
        "time: libraries : {:?} ({} archives, {indexed_classes} top-level names, {} unreadable{})",
        loaded.library_time,
        loaded.libraries.libraries.len(),
        loaded.libraries.failures.len(),
        if loaded.libraries.jdk.is_some() {
            ", JDK included"
        } else {
            ", no JDK"
        },
    );

    let started = Instant::now();
    let mut host = AnalysisHost::default();
    let mut vfs = Vfs::default();
    load::apply_files(&mut host, &mut vfs, &model, loaded.files, |_| false);
    load::apply_libraries(&mut host, loaded.libraries);
    println!("time: database  : {:?}", started.elapsed());
    if let Some(memory) = resident_memory() {
        println!(
            "memory (RSS)    : {:.0} MB after reading",
            memory as f64 / 1_048_576.0
        );
    }

    let started = Instant::now();
    let files: Vec<vfs::FileId> = vfs.existing_files().collect();
    let chunk = files
        .len()
        .div_ceil(rayon::current_num_threads().max(1))
        .max(1);
    // A snapshot per thread: a database handle is sent, never shared.
    let work: Vec<(ide::Analysis, &[vfs::FileId])> = files
        .chunks(chunk)
        .map(|chunk| (host.analysis(), chunk))
        .collect();
    work.into_par_iter().for_each(|(analysis, chunk)| {
        let _ = analysis.prime_item_trees(chunk);
    });
    println!("time: item trees: {:?}", started.elapsed());
    let after_item_trees = resident_memory();

    let started = Instant::now();
    let statistics = match host.analysis().declaration_statistics() {
        Ok(statistics) => statistics,
        Err(_) => {
            eprintln!("kotlin-analyzer: cancelled");
            return ExitCode::FAILURE;
        }
    };
    println!("time: resolution: {:?}", started.elapsed());
    println!("declarations    : {}", statistics.declarations);
    let percent = |count: usize| count as f64 * 100.0 / statistics.type_references.max(1) as f64;
    println!("type references : {}", statistics.type_references);
    println!(
        "  resolved      : {} ({:.1}%)",
        statistics.resolved,
        percent(statistics.resolved)
    );
    println!(
        "  type params   : {} ({:.1}%)",
        statistics.type_parameters,
        percent(statistics.type_parameters)
    );
    println!(
        "  ambiguous     : {} ({:.1}%)",
        statistics.ambiguous,
        percent(statistics.ambiguous)
    );
    println!(
        "  unresolved    : {} ({:.1}%)",
        statistics.unresolved,
        percent(statistics.unresolved)
    );
    println!("  most common ambiguous names:");
    for (name, count) in statistics.top_ambiguous.iter().take(10) {
        println!("    {count:>7}  {name}");
    }
    println!("  most common unresolved names:");
    for (name, count) in statistics.top_unresolved.iter().take(15) {
        println!("    {count:>7}  {name}");
    }
    report_bodies(&host, &vfs, &files);
    println!("time: total     : {:?}", total.elapsed());
    if let Some(memory) = after_item_trees {
        println!(
            "memory (RSS)    : {:.0} MB after item trees",
            memory as f64 / 1_048_576.0
        );
    }
    if let Some(memory) = resident_memory() {
        println!(
            "memory (RSS)    : {:.0} MB at the end",
            memory as f64 / 1_048_576.0
        );
    }
    ExitCode::SUCCESS
}

/// Infers every Kotlin body in parallel and reports what it resolved.
fn report_bodies(host: &AnalysisHost, vfs: &Vfs, files: &[vfs::FileId]) {
    let started = Instant::now();
    let chunk = files
        .len()
        .div_ceil(rayon::current_num_threads().max(1) * 4)
        .max(1);
    let work: Vec<(ide::Analysis, &[vfs::FileId])> = files
        .chunks(chunk)
        .map(|chunk| (host.analysis(), chunk))
        .collect();
    let mut statistics = ide::BodyStatistics::default();
    let parts: Vec<ide::BodyStatistics> = work
        .into_par_iter()
        .filter_map(|(analysis, chunk)| analysis.body_statistics(chunk).ok())
        .collect();
    for part in parts {
        statistics.merge(part);
    }
    println!(
        "time: bodies    : {:?} ({} files)",
        started.elapsed(),
        statistics.files
    );
    let names = statistics.resolved + statistics.candidates + statistics.unresolved;
    let percent = |count: usize| count as f64 * 100.0 / names.max(1) as f64;
    println!("names in bodies : {names}");
    println!(
        "  resolved      : {} ({:.1}%), type known for {:.1}% of them",
        statistics.resolved,
        percent(statistics.resolved),
        statistics.typed as f64 * 100.0 / statistics.resolved.max(1) as f64
    );
    println!(
        "  candidates    : {} ({:.1}%)",
        statistics.candidates,
        percent(statistics.candidates)
    );
    println!(
        "  unresolved    : {} ({:.1}%)",
        statistics.unresolved,
        percent(statistics.unresolved)
    );
    let example = |name: &str| -> String {
        let Some((file, offset)) = statistics.examples.get(name) else {
            return String::new();
        };
        let text = vfs.contents(*file).cloned().unwrap_or_default();
        let position = vfs::LineIndex::new(&text).line_column(&text, *offset as usize);
        format!("{}:{}", vfs.path(*file).display(), position.line + 1)
    };
    println!("  most common unresolved names:");
    for (name, count) in ide::BodyStatistics::ranked(&statistics.unresolved_names)
        .iter()
        .take(15)
    {
        println!("    {count:>7}  {name:<20} e.g. {}", example(name));
    }
    println!("  most common names left with candidates:");
    for (name, count) in ide::BodyStatistics::ranked(&statistics.candidate_names)
        .iter()
        .take(10)
    {
        println!("    {count:>7}  {name:<20} e.g. {}", example(name));
    }
    let mut durations = statistics.durations;
    durations.sort_by_key(|(_, duration)| *duration);
    if let Some((slowest, duration)) = durations.last() {
        let percentile =
            |fraction: f64| durations[((durations.len() - 1) as f64 * fraction) as usize].1;
        println!(
            "  per file      : p50 {:?}, p95 {:?}, max {:?} ({})",
            percentile(0.5),
            percentile(0.95),
            duration,
            vfs.path(*slowest).display()
        );
    }
}

fn distinct_libraries(model: &project_model::ProjectModel) -> usize {
    let mut jars: Vec<&Path> = model
        .source_sets
        .iter()
        .flat_map(|source_set| {
            source_set
                .libraries
                .iter()
                .map(|library| library.jar.as_path())
        })
        .collect();
    jars.sort();
    jars.dedup();
    jars.len()
}

pub(crate) fn resident_memory() -> Option<usize> {
    memory_stats::memory_stats().map(|stats| stats.physical_mem)
}

/// `definition <root> <project.json> <file> <line> <column>` (hidden): the
/// definitions and hover at a position, for debugging.
pub fn definition_command(arguments: &[String]) -> ExitCode {
    let [root, project_json, file, line, column, ..] = arguments else {
        eprintln!("usage: kotlin-analyzer definition <root> <project.json> <file> <line> <column>");
        return ExitCode::FAILURE;
    };
    let (Ok(line), Ok(column)) = (line.parse::<u32>(), column.parse::<u32>()) else {
        eprintln!("line and column are numbers (zero-based)");
        return ExitCode::FAILURE;
    };
    let loaded = match load::read_workspace(Path::new(root), Some(Path::new(project_json))) {
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
    let Some(file_id) = vfs.file_id(Path::new(file)) else {
        eprintln!("{file} is not a workspace file");
        return ExitCode::FAILURE;
    };
    let text = vfs.contents(file_id).cloned().unwrap_or_default();
    let offset = vfs::LineIndex::new(&text).offset(&text, vfs::LineColumn { line, column });
    let analysis = host.analysis();
    match analysis.goto_definition(file_id, offset) {
        Ok(targets) => {
            for target in targets {
                println!("definition: {target:?}");
            }
        }
        Err(_) => println!("cancelled"),
    }
    if let Ok(Some(hover)) = analysis.hover(file_id, offset) {
        println!("hover:\n{}", hover.markdown);
    }
    ExitCode::SUCCESS
}

/// `inference <root> <project.json> <file>`: how long the file's inference
/// takes and the names it leaves unsettled — a debugging aid.
pub fn inference_command(arguments: &[String]) -> ExitCode {
    let [root, project_json, file, ..] = arguments else {
        eprintln!("usage: kotlin-analyzer inference <root> <project.json> <file>");
        return ExitCode::FAILURE;
    };
    let loaded = match load::read_workspace(Path::new(root), Some(Path::new(project_json))) {
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
    let Some(file_id) = vfs.file_id(Path::new(file)) else {
        eprintln!("{file} is not a workspace file");
        return ExitCode::FAILURE;
    };
    let text = vfs.contents(file_id).cloned().unwrap_or_default();
    let index = vfs::LineIndex::new(&text);
    let started = Instant::now();
    let Ok(unsettled) = host.analysis().unsettled_names(file_id) else {
        println!("cancelled");
        return ExitCode::FAILURE;
    };
    println!("time: {:?}", started.elapsed());
    for name in unsettled {
        let position = index.line_column(&text, name.offset as usize);
        let outcome = match name.candidates {
            Some(count) => format!("{count} candidates"),
            None => "unresolved".to_owned(),
        };
        println!(
            "{}:{}  {}  {outcome}",
            position.line + 1,
            position.column + 1,
            name.name
        );
    }
    ExitCode::SUCCESS
}
