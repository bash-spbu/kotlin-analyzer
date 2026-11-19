//! Starting a JVM to debug, or reaching one: the debugger listens on a
//! free port and the JVM's debug agent connects to it
//! (`-agentlib:jdwp=…,server=n,suspend=y`), so no port is fixed and the
//! JVM waits for the debugger before running anything. Gradle runs tests
//! (or a `JavaExec`) with that agent through an init script, so the
//! project's own test configuration — system properties, environment,
//! JVM arguments — applies as in a normal run.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a JVM may take to connect: Gradle builds first.
const CONNECT_PATIENCE: Duration = Duration::from_secs(15 * 60);
/// The most output lines kept.
const OUTPUT_LIMIT: usize = 5000;

/// What to debug.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Launch {
    /// The tests `filter` names (Gradle's `--tests`: `app.OrderTest`,
    /// `app.OrderTest.creates an order`) in the Gradle project at
    /// `project`, in the build at `root`.
    GradleTests {
        root: PathBuf,
        project: PathBuf,
        filter: String,
    },
    /// `java` with `arguments` (the agent goes first), in `directory`.
    Java {
        arguments: Vec<String>,
        directory: PathBuf,
    },
    /// A JVM already listening for a debugger (`server=y`) at `address`.
    Attach { address: String },
}

/// The JVM being debugged — and the process that started it, whose
/// output is kept.
pub struct Debuggee {
    process: Option<Child>,
    output: Arc<Mutex<Vec<String>>>,
}

impl Debuggee {
    /// The output's last `count` lines.
    pub fn output_tail(&self, count: usize) -> Vec<String> {
        let output = self
            .output
            .lock()
            .map(|output| output.clone())
            .unwrap_or_default();
        output[output.len().saturating_sub(count)..].to_vec()
    }

    /// Waits up to `patience` for the process to end: its exit code.
    pub fn wait(&mut self, patience: Duration) -> Option<i32> {
        let process = self.process.as_mut()?;
        let deadline = Instant::now() + patience;
        loop {
            if let Ok(Some(status)) = process.try_wait() {
                return status.code();
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn kill(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

impl Drop for Debuggee {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Starts or reaches the JVM: a connection to it (handshake done) and
/// the process.
pub fn start(launch: &Launch) -> Result<(TcpStream, Debuggee), String> {
    if let Launch::Attach { address } = launch {
        let stream = TcpStream::connect(address)
            .map_err(|error| format!("can't reach {address}: {error}"))?;
        return Ok((
            stream,
            Debuggee {
                process: None,
                output: Arc::default(),
            },
        ));
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let agent = agent_argument(port);
    let mut command = match launch {
        Launch::GradleTests {
            root,
            project,
            filter,
        } => gradle_tests(root, project, filter, &agent)?,
        Launch::Java {
            arguments,
            directory,
        } => {
            let mut command = Command::new(java());
            command.arg(&agent).args(arguments).current_dir(directory);
            command
        }
        Launch::Attach { .. } => unreachable!("attached above"),
    };
    let mut process = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the program: {error}"))?;
    let output: Arc<Mutex<Vec<String>>> = Arc::default();
    if let Some(stdout) = process.stdout.take() {
        keep_lines(stdout, output.clone());
    }
    if let Some(stderr) = process.stderr.take() {
        keep_lines(stderr, output.clone());
    }
    let mut debuggee = Debuggee {
        process: Some(process),
        output,
    };
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                return Ok((stream, debuggee));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.to_string()),
        }
        if let Some(code) = debuggee.wait(Duration::ZERO) {
            let tail = debuggee.output_tail(30).join("\n");
            return Err(format!(
                "the program ended (exit code {code}) before the JVM connected:\n{tail}"
            ));
        }
        if Instant::now() > deadline {
            debuggee.kill();
            return Err("the JVM didn't connect in time".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The debug agent's option: connect to the debugger at `port` and wait
/// for it before running.
pub fn agent_argument(port: u16) -> String {
    format!("-agentlib:jdwp=transport=dt_socket,server=n,suspend=y,address=127.0.0.1:{port}")
}

/// `gradlew -p <project> test --tests <filter> --rerun` with an init
/// script giving the test JVM the agent (one fork, so one JVM connects).
fn gradle_tests(root: &Path, project: &Path, filter: &str, agent: &str) -> Result<Command, String> {
    let script = std::env::temp_dir().join(format!(
        "kotlin-analyzer-debug-{}-{}.gradle",
        std::process::id(),
        agent.rsplit(':').next().unwrap_or("0")
    ));
    std::fs::write(&script, init_script(agent)).map_err(|error| error.to_string())?;
    let wrapper = root.join("gradlew");
    let mut command = if wrapper.is_file() {
        Command::new(wrapper)
    } else {
        Command::new("gradle")
    };
    let project = project.strip_prefix(root).unwrap_or(project);
    if !project.as_os_str().is_empty() {
        command.arg("-p").arg(project);
    }
    command
        .args([
            "test",
            "--tests",
            filter,
            "--rerun",
            "--console=plain",
            "-I",
        ])
        .arg(&script)
        .current_dir(root);
    Ok(command)
}

/// The init script: every `Test` and `JavaExec` task runs its JVM with
/// the agent; tests in one fork. The agent goes through
/// `jvmArgumentProviders`, which a build script's `jvmArgs = [...]`
/// doesn't replace.
fn init_script(agent: &str) -> String {
    format!(
        "def kotlinAnalyzerAgent = {{ ['{agent}'] }} as org.gradle.process.CommandLineArgumentProvider\n\
         allprojects {{\n\
         \x20   tasks.withType(Test).configureEach {{\n\
         \x20       jvmArgumentProviders.add(kotlinAnalyzerAgent)\n\
         \x20       maxParallelForks = 1\n\
         \x20       forkEvery = 0\n\
         \x20   }}\n\
         \x20   tasks.withType(JavaExec).configureEach {{\n\
         \x20       jvmArgumentProviders.add(kotlinAnalyzerAgent)\n\
         \x20   }}\n\
         }}\n"
    )
}

fn java() -> PathBuf {
    std::env::var_os("JAVA_HOME")
        .map(|home| Path::new(&home).join("bin").join("java"))
        .filter(|java| java.is_file())
        .unwrap_or_else(|| PathBuf::from("java"))
}

fn keep_lines(stream: impl std::io::Read + Send + 'static, output: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Ok(mut output) = output.lock() {
                output.push(line);
                if output.len() > OUTPUT_LIMIT {
                    let excess = output.len() - OUTPUT_LIMIT;
                    output.drain(..excess);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_script_gives_tests_and_java_runs_the_agent_in_one_fork() {
        let script = init_script(&agent_argument(5123));
        assert!(script.contains(
            "{ ['-agentlib:jdwp=transport=dt_socket,server=n,suspend=y,address=127.0.0.1:5123'] }"
        ));
        assert_eq!(
            script
                .matches("jvmArgumentProviders.add(kotlinAnalyzerAgent)")
                .count(),
            2
        );
        assert!(
            !script.contains("jvmArgs"),
            "a build's `jvmArgs = [...]` would replace it"
        );
        assert!(script.contains("maxParallelForks = 1"));
        assert!(script.contains("tasks.withType(JavaExec)"));
    }
}
