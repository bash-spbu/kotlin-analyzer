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

    let started = Instant::now();
    let mut host = AnalysisHost::default();
    let mut vfs = Vfs::default();
    load::apply_files(&mut host, &mut vfs, &model, loaded.files, |_| false);
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
    println!("  most common unresolved names (libraries come in M2):");
    for (name, count) in statistics.top_unresolved.iter().take(15) {
        println!("    {count:>7}  {name}");
    }
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

fn resident_memory() -> Option<usize> {
    memory_stats::memory_stats().map(|stats| stats.physical_mem)
}
