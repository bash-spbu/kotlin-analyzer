//! Reads every JAR listed in a `project.json` and reports totals:
//! `cargo run --release -p classfile --example read_jars -- <project.json>`.
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("a project.json");
    let text = std::fs::read_to_string(path).expect("readable");
    let mut jars: Vec<String> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("\"jar\": \""))
        .map(|rest| rest.trim_end_matches(['"', ',']).to_owned())
        .collect();
    jars.sort();
    jars.dedup();
    let started = Instant::now();
    let (mut classes, mut members, mut failures) = (0usize, 0usize, 0usize);
    let results: Vec<_> = std::thread::scope(|scope| {
        let chunks: Vec<&[String]> = jars.chunks(jars.len().div_ceil(8).max(1)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|jar| classfile::read_jar(std::path::Path::new(jar)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    for result in results {
        match result {
            Ok(contents) => {
                classes += contents.classes.len();
                members += contents
                    .classes
                    .iter()
                    .map(|class| class.members.len())
                    .sum::<usize>()
                    + contents
                        .package_members
                        .iter()
                        .map(|part| part.members.len())
                        .sum::<usize>();
            }
            Err(_) => failures += 1,
        }
    }
    println!(
        "{} jars, {classes} classes, {members} members, {failures} unreadable, {:?}",
        jars.len(),
        started.elapsed()
    );
}
