//! `textDocument/formatting` by an external formatter: ktrs (the default)
//! or ktfmt reads the document on stdin and writes it formatted on stdout;
//! the difference is one edit. What the formatter says about a file it
//! can't format goes to the user, never to the log: it may quote the code.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use lsp_server::{ErrorCode, Request, Response};
use serde_json::Value;
use vfs::LineIndex;

use super::convert;
use super::handlers::Snapshot;
use crate::log::Event;

/// How long a formatter may take: ktfmt's jar starts a JVM.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The most lines of a formatter's complaint shown to the user.
const COMPLAINT_LINES: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Formatter {
    /// ktfmt's output, natively: <https://github.com/Hexay/ktrs>.
    Ktrs,
    /// The original: <https://github.com/facebook/ktfmt>.
    Ktfmt,
}

impl Formatter {
    pub(crate) fn from_setting(setting: &str) -> Option<Formatter> {
        match setting {
            "ktrs" => Some(Formatter::Ktrs),
            "ktfmt" => Some(Formatter::Ktfmt),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Formatter::Ktrs => "ktrs",
            Formatter::Ktfmt => "ktfmt",
        }
    }

    /// The command line after the program: format stdin in `style`.
    fn arguments(self, style: Style) -> Vec<&'static str> {
        match self {
            Formatter::Ktrs => vec!["fmt", "--style", style.name(), "-"],
            Formatter::Ktfmt => {
                let style_flag = match style {
                    Style::Kotlinlang => "--kotlinlang-style",
                    Style::Meta => "--meta-style",
                    Style::Google => "--google-style",
                };
                vec![style_flag, "-"]
            }
        }
    }
}

/// ktfmt's styles, which ktrs has too.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Style {
    /// Kotlin's coding conventions: four-space indents.
    #[default]
    Kotlinlang,
    /// ktfmt's own default: two-space indents.
    Meta,
    Google,
}

impl Style {
    pub(crate) fn from_setting(setting: &str) -> Option<Style> {
        match setting {
            "kotlinlang" => Some(Style::Kotlinlang),
            "meta" => Some(Style::Meta),
            "google" => Some(Style::Google),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Style::Kotlinlang => "kotlinlang",
            Style::Meta => "meta",
            Style::Google => "google",
        }
    }
}

/// Where the formatter is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Program {
    /// Not said: the formatter's name, looked up on `PATH`.
    OnPath,
    Executable(PathBuf),
    /// A jar (ktfmt's `-with-dependencies` one), run by `java`.
    Jar(PathBuf),
}

impl Program {
    /// The program at `path`: a jar by its extension.
    pub(crate) fn at(path: PathBuf) -> Program {
        let is_jar = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"));
        if is_jar {
            Program::Jar(path)
        } else {
            Program::Executable(path)
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Program::OnPath => "on-path",
            Program::Executable(_) => "executable",
            Program::Jar(_) => "jar",
        }
    }
}

/// How documents are formatted, from the client's settings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum FormattingConfig {
    /// Formatting is left to something else: requests get no edits.
    Off,
    On(FormatterSetup),
}

impl Default for FormattingConfig {
    fn default() -> FormattingConfig {
        FormattingConfig::On(FormatterSetup {
            formatter: Formatter::Ktrs,
            style: Style::default(),
            program: Program::OnPath,
        })
    }
}

impl FormattingConfig {
    /// For the log: the formatter's name, or `off`.
    pub(crate) fn formatter_name(&self) -> &'static str {
        match self {
            FormattingConfig::Off => "off",
            FormattingConfig::On(setup) => setup.formatter.name(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FormatterSetup {
    pub(crate) formatter: Formatter,
    pub(crate) style: Style,
    pub(crate) program: Program,
}

impl FormatterSetup {
    fn command(&self) -> Command {
        let mut command = match &self.program {
            Program::OnPath => Command::new(self.formatter.name()),
            Program::Executable(path) => Command::new(path),
            Program::Jar(path) => {
                let mut java = Command::new(java());
                java.arg("-jar").arg(path);
                java
            }
        };
        command.args(self.formatter.arguments(self.style));
        command
    }

    /// What runs, for the user: the formatter, or `java` for its jar.
    fn program_name(&self) -> &'static str {
        match self.program {
            Program::Jar(_) => "java",
            Program::OnPath | Program::Executable(_) => self.formatter.name(),
        }
    }
}

/// `$JAVA_HOME/bin/java` when there is one, else `java` on `PATH`.
fn java() -> PathBuf {
    let from_java_home = std::env::var_os("JAVA_HOME")
        .map(|home| Path::new(&home).join("bin").join("java"))
        .filter(|java| java.is_file());
    from_java_home.unwrap_or_else(|| PathBuf::from("java"))
}

/// Why a formatter gave no text.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// The program isn't there.
    NotFound,
    NotStarted(std::io::ErrorKind),
    TimedOut,
    /// It ran and refused the file (a syntax error, say), saying why.
    Rejected {
        exit_code: Option<i32>,
        complaint: String,
    },
    NotUtf8,
}

impl Failure {
    fn outcome(&self) -> &'static str {
        match self {
            Failure::NotFound => "not-found",
            Failure::NotStarted(_) => "not-started",
            Failure::TimedOut => "timed-out",
            Failure::Rejected { .. } => "rejected",
            Failure::NotUtf8 => "not-utf8",
        }
    }

    fn message(&self, setup: &FormatterSetup) -> String {
        let formatter = setup.formatter.name();
        let program = setup.program_name();
        match self {
            Failure::NotFound if matches!(setup.program, Program::Jar(_)) => format!(
                "kotlin-analyzer: `java` wasn't found to run {formatter}'s jar; set JAVA_HOME"
            ),
            Failure::NotFound => format!(
                "kotlin-analyzer: {formatter} wasn't found; put it on PATH or set `formatting.path`"
            ),
            Failure::NotStarted(kind) => {
                format!("kotlin-analyzer: {program} couldn't be started: {kind}")
            }
            Failure::TimedOut => format!(
                "kotlin-analyzer: {formatter} took longer than {} s and was stopped",
                TIMEOUT.as_secs()
            ),
            Failure::Rejected { complaint, .. } if complaint.is_empty() => {
                format!("{formatter} couldn't format this file")
            }
            Failure::Rejected { complaint, .. } => {
                format!("{formatter} couldn't format this file:\n{complaint}")
            }
            Failure::NotUtf8 => format!("kotlin-analyzer: {formatter} wrote text that isn't UTF-8"),
        }
    }
}

pub(super) fn handle(snapshot: Snapshot, request: Request) -> Response {
    let id = request.id;
    let Some(file) = snapshot.request_file else {
        return Response::new_ok(id, Value::Null);
    };
    let FormattingConfig::On(setup) = snapshot.formatting.as_ref() else {
        return Response::new_ok(id, Value::Null);
    };
    let is_kotlin = snapshot
        .paths
        .get(file.index())
        .is_some_and(|path| is_kotlin(path));
    if !is_kotlin {
        return Response::new_ok(id, Value::Null);
    }
    let text = match snapshot.analysis.file_text(file) {
        Ok(Some(text)) => text,
        Ok(None) => return Response::new_ok(id, Value::Null),
        Err(_cancelled) => {
            return Response::new_err(
                id,
                ErrorCode::ContentModified as i32,
                "content modified".to_owned(),
            );
        }
    };
    let started = Instant::now();
    let formatted = run(setup.command(), &text, TIMEOUT);
    let event = Event::debug("formatted")
        .text("formatter", setup.formatter.name())
        .text("style", setup.style.name())
        .text("program", setup.program.kind())
        .duration("took", started.elapsed());
    match formatted {
        Ok(formatted) => {
            let replacement = Replacement::between(&text, &formatted);
            event
                .text("outcome", "formatted")
                .count(
                    "replaced",
                    replacement
                        .as_ref()
                        .map_or(0, |replacement| replacement.range.len()),
                )
                .emit();
            let edits: Vec<lsp_types::TextEdit> = replacement
                .into_iter()
                .map(|replacement| replacement.edit(&text, snapshot.encoding))
                .collect();
            Response::new_ok(id, edits)
        }
        Err(failure) => {
            let mut event = event.text("outcome", failure.outcome());
            if let Failure::Rejected {
                exit_code: Some(exit_code),
                ..
            } = failure
            {
                event = event.number("exit-code", i64::from(exit_code));
            }
            event.emit();
            Response::new_err(id, ErrorCode::RequestFailed as i32, failure.message(setup))
        }
    }
}

fn is_kotlin(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "kt" || extension == "kts")
}

/// `input` through `command`'s stdin, its stdout as text; it's stopped
/// after `timeout`.
fn run(mut command: Command, input: &str, timeout: Duration) -> Result<String, Failure> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotFound,
            kind => Failure::NotStarted(kind),
        })?;
    // Written and read on threads of their own: a formatter may write
    // before it has read everything, and a full pipe would stop both.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = input.as_bytes().to_vec();
    std::thread::spawn(move || {
        // A formatter that exits without reading is answered by its status.
        let _ = stdin.write_all(&input);
    });
    let stdout = read_on_thread(child.stdout.take().expect("stdout is piped"));
    let stderr = read_on_thread(child.stderr.take().expect("stderr is piped"));
    let status = wait(&mut child, timeout)?;
    let output = stdout.join().unwrap_or_default();
    if !status.success() {
        let complaint = String::from_utf8_lossy(&stderr.join().unwrap_or_default())
            .lines()
            .take(COMPLAINT_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Failure::Rejected {
            exit_code: status.code(),
            complaint,
        });
    }
    String::from_utf8(output).map_err(|_| Failure::NotUtf8)
}

fn read_on_thread(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// The child's exit status, or `TimedOut` (and the child killed) once
/// `timeout` has passed.
fn wait(child: &mut Child, timeout: Duration) -> Result<ExitStatus, Failure> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::TimedOut);
            }
            Err(error) => {
                let _ = child.kill();
                return Err(Failure::NotStarted(error.kind()));
            }
        }
    }
}

/// The one edit that turns a text into its formatted form: what lies
/// between their common start and their common end.
#[derive(Debug, PartialEq, Eq)]
struct Replacement {
    /// Byte offsets in the old text.
    range: std::ops::Range<usize>,
    text: String,
}

impl Replacement {
    /// `None` when the texts are the same.
    fn between(old: &str, new: &str) -> Option<Replacement> {
        if old == new {
            return None;
        }
        let prefix: usize = old
            .chars()
            .zip(new.chars())
            .take_while(|(old_char, new_char)| old_char == new_char)
            .map(|(char, _)| char.len_utf8())
            .sum();
        // The common end, within what follows the common start in both.
        let suffix: usize = old[prefix..]
            .chars()
            .rev()
            .zip(new[prefix..].chars().rev())
            .take_while(|(old_char, new_char)| old_char == new_char)
            .map(|(char, _)| char.len_utf8())
            .sum();
        Some(Replacement {
            range: prefix..old.len() - suffix,
            text: new[prefix..new.len() - suffix].to_owned(),
        })
    }

    fn edit(&self, old: &str, unit: vfs::ColumnUnit) -> lsp_types::TextEdit {
        let index = LineIndex::with_unit(old, unit);
        lsp_types::TextEdit {
            range: lsp_types::Range::new(
                convert::position(&index, old, self.range.start),
                convert::position(&index, old, self.range.end),
            ),
            new_text: self.text.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, replacement: &Replacement) -> String {
        let mut text = old.to_owned();
        text.replace_range(replacement.range.clone(), &replacement.text);
        text
    }

    #[test]
    fn the_edit_spans_only_what_formatting_changed() {
        let old = "fun main() {\n  val x=1\n  println(x)\n}\n";
        let new = "fun main() {\n    val x = 1\n    println(x)\n}\n";
        let replacement = Replacement::between(old, new).unwrap();
        assert_eq!(&old[replacement.range.clone()], "val x=1\n");
        assert_eq!(replacement.text, "  val x = 1\n  ");
        assert_eq!(apply(old, &replacement), new);
        assert_eq!(Replacement::between(old, old), None);
    }

    #[test]
    fn the_edit_keeps_to_whole_characters_and_counts_columns_in_the_clients_unit() {
        // `é` and `è` share their first UTF-8 byte: a byte comparison would
        // split them.
        let old = "val 😀 = \"é\"\n";
        let new = "val 😀 = \"è\"\n";
        let replacement = Replacement::between(old, new).unwrap();
        assert_eq!(apply(old, &replacement), new);
        assert_eq!(replacement.text, "è");
        let utf16 = replacement.edit(old, vfs::ColumnUnit::Utf16);
        assert_eq!(utf16.range.start, lsp_types::Position::new(0, 10));
        let utf8 = replacement.edit(old, vfs::ColumnUnit::Utf8);
        assert_eq!(utf8.range.start, lsp_types::Position::new(0, 12));
        // Repeated text: the common start and end don't overlap.
        let repeated = Replacement::between("aaa", "aaaa").unwrap();
        assert_eq!(apply("aaa", &repeated), "aaaa");
    }

    #[test]
    fn each_formatter_is_asked_for_the_style_on_stdin() {
        let ktrs = FormatterSetup {
            formatter: Formatter::Ktrs,
            style: Style::Kotlinlang,
            program: Program::OnPath,
        };
        let command = ktrs.command();
        assert_eq!(command.get_program(), "ktrs");
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, ["fmt", "--style", "kotlinlang", "-"]);

        let ktfmt = FormatterSetup {
            formatter: Formatter::Ktfmt,
            style: Style::Google,
            program: Program::at(PathBuf::from("/tools/ktfmt-0.65-with-dependencies.jar")),
        };
        let command = ktfmt.command();
        assert!(command.get_program().to_string_lossy().ends_with("java"));
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(
            arguments,
            [
                "-jar",
                "/tools/ktfmt-0.65-with-dependencies.jar",
                "--google-style",
                "-"
            ]
        );
        assert_eq!(
            Program::at(PathBuf::from("/tools/ktfmt")),
            Program::Executable(PathBuf::from("/tools/ktfmt"))
        );
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command
    }

    #[test]
    fn a_formatter_gets_the_text_on_stdin_and_its_failures_are_told_apart() {
        let big = "x".repeat(1 << 18);
        assert_eq!(run(shell("cat"), &big, TIMEOUT).unwrap(), big);
        assert_eq!(
            run(shell("echo 'line 1: bad' >&2; exit 3"), "", TIMEOUT),
            Err(Failure::Rejected {
                exit_code: Some(3),
                complaint: "line 1: bad".to_owned()
            })
        );
        assert_eq!(
            run(shell("sleep 5"), "", Duration::from_millis(100)),
            Err(Failure::TimedOut)
        );
        assert_eq!(
            run(Command::new("/no/such/formatter"), "", TIMEOUT),
            Err(Failure::NotFound)
        );
        assert_eq!(
            run(shell("printf '\\377'"), "", TIMEOUT),
            Err(Failure::NotUtf8)
        );
    }
}
