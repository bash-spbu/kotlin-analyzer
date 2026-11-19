//! The debugger against Kotlin's bytecode: inline functions (their
//! bodies' source maps), conditional breakpoints and logpoints, and
//! coroutines. The program is compiled by the Kotlin compiler Gradle
//! keeps in its cache — skipped where there is none.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use debugger::{BreakpointOptions, Frame, FrameKind, Launch, Session, Stop};
use jdwp::StepDepth;

const MAIN: &str = r#"package demo

import demo.util.logged
import kotlin.coroutines.*

inline fun twice(block: (Int) -> Unit) {
    block(1)
    block(2)
}

var waiting: Continuation<Int>? = null

suspend fun pause(): Int = suspendCoroutine { continuation -> waiting = continuation }

suspend fun work(start: Int): Int {
    val first = start + 1
    val resumed = pause()
    val second = first + resumed
    return second
}

suspend fun outer(): Int {
    val result = work(5)
    return result * 2
}

fun main() {
    val items = listOf(3, 4, 5)
    var sum = 0
    items.forEach { item ->
        sum += item
    }
    twice { sum += it }
    val logged = logged { sum * 10 }
    ::outer.startCoroutine(Continuation(EmptyCoroutineContext) { println("done ${it.getOrNull()}") })
    waiting!!.resume(10)
    println("sum $sum $logged")
}
"#;

const HELPERS: &str = r#"package demo.util

inline fun <T> logged(block: () -> T): T {
    val started = 1
    val result = block()
    println("logged $result after $started")
    return result
}
"#;

const PATIENCE: Duration = Duration::from_secs(60);

/// The compiled program: its root (sources in `src`, classes in
/// `classes`) and the classpath to run it with.
struct Compiled {
    root: PathBuf,
    classpath: String,
}

impl Compiled {
    fn main_file(&self) -> PathBuf {
        self.root.join("src/demo/Main.kt")
    }

    fn helpers_file(&self) -> PathBuf {
        self.root.join("src/demo/util/Helpers.kt")
    }

    fn start(&self) -> Session {
        let launch = Launch::Java {
            arguments: vec![
                "-cp".to_owned(),
                self.classpath.clone(),
                "demo.MainKt".to_owned(),
            ],
            directory: self.root.clone(),
            environment: Vec::new(),
        };
        Session::start(&launch, &[self.root.join("src")]).unwrap()
    }
}

/// A jar of Gradle's cache: `<group>/<artifact>/<version>/<hash>/<artifact>-<version>.jar`.
fn cached_jar(cache: &Path, group: &str, artifact: &str, version: &str) -> Option<PathBuf> {
    let versions = cache.join(group).join(artifact).join(version);
    let file_name = format!("{artifact}-{version}.jar");
    std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .map(|hash| hash.path().join(&file_name))
        .find(|jar| jar.is_file())
}

/// Some version of `artifact` in the cache.
fn any_cached_jar(cache: &Path, group: &str, artifact: &str) -> Option<PathBuf> {
    let mut versions: Vec<String> = std::fs::read_dir(cache.join(group).join(artifact))
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    versions.sort();
    versions
        .iter()
        .rev()
        .find_map(|version| cached_jar(cache, group, artifact, version))
}

/// The compiler's jars and the standard library, of one version.
fn kotlin_compiler(cache: &Path) -> Option<(Vec<PathBuf>, PathBuf)> {
    const KOTLIN: &str = "org.jetbrains.kotlin";
    let mut versions: Vec<String> =
        std::fs::read_dir(cache.join(KOTLIN).join("kotlin-compiler-embeddable"))
            .ok()?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
    versions.sort();
    versions.iter().rev().find_map(|version| {
        let mut jars = Vec::new();
        for artifact in [
            "kotlin-compiler-embeddable",
            "kotlin-stdlib",
            "kotlin-script-runtime",
            "kotlin-reflect",
            "kotlin-daemon-embeddable",
        ] {
            jars.push(cached_jar(cache, KOTLIN, artifact, version)?);
        }
        jars.push(any_cached_jar(
            cache,
            "org.jetbrains.intellij.deps",
            "trove4j",
        )?);
        jars.push(any_cached_jar(
            cache,
            "org.jetbrains.kotlinx",
            "kotlinx-coroutines-core-jvm",
        )?);
        jars.push(any_cached_jar(cache, "org.jetbrains", "annotations")?);
        let standard_library = jars[1].clone();
        Some((jars, standard_library))
    })
}

fn compiled() -> Option<&'static Compiled> {
    static COMPILED: OnceLock<Option<Compiled>> = OnceLock::new();
    COMPILED
        .get_or_init(|| {
            let home = std::env::var_os("HOME")?;
            let cache = std::env::var_os("GRADLE_USER_HOME")
                .map_or_else(|| PathBuf::from(&home).join(".gradle"), PathBuf::from)
                .join("caches/modules-2/files-2.1");
            let (compiler, standard_library) = kotlin_compiler(&cache)?;
            let root =
                std::env::temp_dir().join(format!("kotlin-analyzer-kotlin-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src/demo/util")).unwrap();
            std::fs::write(root.join("src/demo/Main.kt"), MAIN).unwrap();
            std::fs::write(root.join("src/demo/util/Helpers.kt"), HELPERS).unwrap();
            let compiler_classpath = std::env::join_paths(&compiler).unwrap();
            let status = Command::new("java")
                .arg("-cp")
                .arg(&compiler_classpath)
                .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
                .arg("-no-stdlib")
                .arg("-cp")
                .arg(&standard_library)
                .arg("-d")
                .arg(root.join("classes"))
                .arg(root.join("src/demo/Main.kt"))
                .arg(root.join("src/demo/util/Helpers.kt"))
                .stderr(std::process::Stdio::null())
                .status()
                .ok()?;
            assert!(status.success(), "kotlinc failed");
            let classpath = std::env::join_paths([root.join("classes"), standard_library])
                .unwrap()
                .to_string_lossy()
                .into_owned();
            Some(Compiled { root, classpath })
        })
        .as_ref()
}

fn line_of(text: &str, needle: &str) -> u32 {
    text.lines().position(|line| line.contains(needle)).unwrap() as u32 + 1
}

fn shown(session: &mut Session, thread: jdwp::ThreadId, frame: &Frame) -> Vec<String> {
    session
        .variables(thread, frame)
        .unwrap()
        .into_iter()
        .map(|variable| format!("{} = {}", variable.name, variable.value))
        .collect()
}

fn stopped_thread(stop: Option<Stop>) -> jdwp::ThreadId {
    let stop = stop.expect("stopped in time");
    stop.thread().unwrap_or_else(|| panic!("{stop:?}"))
}

#[test]
fn inline_bodies_are_frames_of_their_own_and_steps_skip_the_libraries() {
    let Some(compiled) = compiled() else {
        eprintln!("no Kotlin compiler in Gradle's cache: skipped");
        return;
    };
    let main = compiled.main_file();
    let helpers = compiled.helpers_file();
    let mut session = compiled.start();
    session
        .set_breakpoint(&main, line_of(MAIN, "items.forEach"))
        .unwrap();
    // In `twice`'s body: the same file's code, inlined at its call.
    session
        .set_breakpoint(&main, line_of(MAIN, "block(1)"))
        .unwrap();
    // In another package's file: its code is only in `MainKt`.
    let in_helpers = session
        .set_breakpoint(&helpers, line_of(HELPERS, "val started"))
        .unwrap();

    let thread = stopped_thread(session.continue_to_stop(PATIENCE).unwrap());
    let top = session.top_frame(thread).unwrap();
    assert_eq!(top.line, Some(line_of(MAIN, "items.forEach") as i32));
    // Over `forEach`: not into its inlined body (no source), to the lambda.
    let stop = session.step(thread, StepDepth::Over, PATIENCE).unwrap();
    assert!(matches!(stop, Some(Stop::Step { .. })), "{stop:?}");
    let top = session.top_frame(thread).unwrap();
    assert_eq!(
        (top.kind.clone(), top.line, top.path.as_deref()),
        (
            FrameKind::Code,
            Some(line_of(MAIN, "sum += item") as i32),
            Some(main.as_path())
        )
    );
    let variables = shown(&mut session, thread, &top);
    assert!(variables.contains(&"item = 3".to_owned()), "{variables:?}");
    assert!(
        !variables.iter().any(|variable| variable.contains("$iv")),
        "the inlined body's locals aren't the caller's: {variables:?}"
    );

    let thread = stopped_thread(session.continue_to_stop(PATIENCE).unwrap());
    let stack = session.stack(thread).unwrap();
    assert_eq!(
        (
            stack[0].kind.clone(),
            stack[0].line,
            stack[0].path.as_deref()
        ),
        (
            FrameKind::Inlined {
                function: Some("twice".to_owned())
            },
            Some(line_of(MAIN, "block(1)") as i32),
            Some(main.as_path())
        )
    );
    assert_eq!(
        (
            stack[1].kind.clone(),
            stack[1].method.as_str(),
            stack[1].line
        ),
        (
            FrameKind::Code,
            "main",
            Some(line_of(MAIN, "twice { sum") as i32)
        )
    );
    let variables = shown(&mut session, thread, &stack[1]);
    assert!(variables.contains(&"sum = 12".to_owned()), "{variables:?}");

    let stop = session.continue_to_stop(PATIENCE).unwrap();
    assert!(
        matches!(&stop, Some(Stop::Breakpoint { breakpoints, .. }) if breakpoints == &[in_helpers.id]),
        "{stop:?}"
    );
    let thread = stopped_thread(stop);
    let stack = session.stack(thread).unwrap();
    assert_eq!(
        (
            stack[0].kind.clone(),
            stack[0].line,
            stack[0].path.as_deref()
        ),
        (
            FrameKind::Inlined {
                function: Some("logged".to_owned())
            },
            Some(line_of(HELPERS, "val started") as i32),
            Some(helpers.as_path())
        )
    );
    assert_eq!(stack[1].line, Some(line_of(MAIN, "val logged") as i32));
    // Over, inside the body it began in: it stays there.
    session.step(thread, StepDepth::Over, PATIENCE).unwrap();
    let top = session.top_frame(thread).unwrap();
    assert_eq!(
        (top.line, top.path.as_deref()),
        (
            Some(line_of(HELPERS, "val result") as i32),
            Some(helpers.as_path())
        )
    );
    let variables = shown(&mut session, thread, &top);
    assert_eq!(
        variables,
        ["started = 1"],
        "the body's own locals, named as in its source"
    );
    assert_eq!(
        session.evaluate(thread, &top, "started").unwrap().value,
        "1"
    );
    session.terminate();
}

#[test]
fn conditions_hit_counts_and_logpoints_decide_whether_to_stop() {
    let Some(compiled) = compiled() else {
        eprintln!("no Kotlin compiler in Gradle's cache: skipped");
        return;
    };
    let main = compiled.main_file();
    let mut session = compiled.start();
    let line = line_of(MAIN, "sum += item");
    assert!(
        session
            .set_breakpoint_with(
                &main,
                line,
                BreakpointOptions {
                    condition: Some("item >".to_owned()),
                    ..BreakpointOptions::default()
                }
            )
            .is_err(),
        "a condition that doesn't parse"
    );
    session
        .set_breakpoint_with(
            &main,
            line,
            BreakpointOptions {
                log_message: Some("item {item}, sum {sum}".to_owned()),
                ..BreakpointOptions::default()
            },
        )
        .unwrap();
    let conditional = session
        .set_breakpoint_with(
            &main,
            line,
            BreakpointOptions {
                condition: Some("item >= 4 && sum > 0".to_owned()),
                // Of the hits the condition lets through, the second.
                hit_condition: Some("2".to_owned()),
                log_message: None,
            },
        )
        .unwrap();
    let stop = session.continue_to_stop(PATIENCE).unwrap();
    assert!(
        matches!(&stop, Some(Stop::Breakpoint { breakpoints, .. }) if breakpoints == &[conditional.id]),
        "{stop:?}"
    );
    let thread = stopped_thread(stop);
    let top = session.top_frame(thread).unwrap();
    let variables = shown(&mut session, thread, &top);
    assert!(variables.contains(&"item = 5".to_owned()), "{variables:?}");
    assert!(variables.contains(&"sum = 7".to_owned()), "{variables:?}");
    let output = session.output_tail(10);
    assert_eq!(
        output,
        ["item 3, sum 0", "item 4, sum 3", "item 5, sum 7"],
        "each hit logged, the stopping one too"
    );
    session.terminate();
}

#[test]
fn coroutines_show_who_waits_and_a_step_over_a_suspension_ends_where_it_resumes() {
    let Some(compiled) = compiled() else {
        eprintln!("no Kotlin compiler in Gradle's cache: skipped");
        return;
    };
    let main = compiled.main_file();
    let mut session = compiled.start();
    let suspends = line_of(MAIN, "val resumed = pause()");
    let after = line_of(MAIN, "val second");
    session.set_breakpoint(&main, suspends).unwrap();
    let thread = stopped_thread(session.continue_to_stop(PATIENCE).unwrap());
    assert_eq!(
        session.top_frame(thread).unwrap().line,
        Some(suspends as i32)
    );

    // `pause()` suspends; `main` resumes the coroutine with 10.
    let stop = session.step(thread, StepDepth::Over, PATIENCE).unwrap();
    assert!(matches!(stop, Some(Stop::Step { .. })), "{stop:?}");
    let thread = stopped_thread(stop);
    let stack = session.stack(thread).unwrap();
    assert_eq!(
        (stack[0].method.as_str(), stack[0].line),
        ("work", Some(after as i32))
    );
    let variables = shown(&mut session, thread, &stack[0]);
    assert!(
        variables.contains(&"resumed = 10".to_owned()),
        "{variables:?}"
    );
    assert!(variables.contains(&"first = 6".to_owned()), "{variables:?}");
    // `outer` waits for `work`: not on the thread, in the coroutine.
    let waiting = stack
        .iter()
        .find(|frame| matches!(frame.kind, FrameKind::Async { .. }))
        .unwrap_or_else(|| panic!("no async frame in {stack:#?}"));
    assert_eq!(
        (
            waiting.method.as_str(),
            waiting.line,
            waiting.path.as_deref()
        ),
        (
            "outer",
            Some(line_of(MAIN, "val result = work(5)") as i32),
            Some(main.as_path())
        )
    );
    assert!(session.variables(thread, waiting).is_ok());

    let stop = session.continue_to_stop(PATIENCE).unwrap();
    assert_eq!(stop, Some(Stop::Ended { exit_code: Some(0) }));
    let output = session.output_tail(10);
    assert!(output.contains(&"done 32".to_owned()), "{output:?}");
    session.terminate();
}
