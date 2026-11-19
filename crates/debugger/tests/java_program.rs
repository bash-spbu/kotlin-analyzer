//! The debugger against a real JVM: a small program compiled by `javac`
//! (skipped where there is none), run with the debug agent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use debugger::{Launch, Session, Stop};
use jdwp::StepDepth;

const PROGRAM: &str = r#"package demo;

import java.util.ArrayList;
import java.util.List;

public class Main {
    enum Status { ACTIVE, CLOSED }

    static class Order {
        final String id;
        final List<Integer> items = new ArrayList<>();
        Status status = Status.ACTIVE;
        Order(String id) { this.id = id; }
        int total() {
            int sum = 0;
            for (int item : items) {
                sum += item;
            }
            return sum;
        }
    }

    public static void main(String[] arguments) {
        Order order = new Order("A-1");
        order.items.add(3);
        order.items.add(4);
        int[] numbers = {1, 2, 3};
        java.math.BigDecimal price = new java.math.BigDecimal("1.50");
        int total = order.total();
        System.out.println("total " + total);
        if (total > 5) {
            throw new IllegalStateException("too much: " + total);
        }
    }
}
"#;

const PATIENCE: Duration = Duration::from_secs(60);

/// The program compiled with debug information, or `None` without `javac`.
fn compiled(name: &str) -> Option<(PathBuf, PathBuf)> {
    let root = std::env::temp_dir().join(format!(
        "kotlin-analyzer-debugger-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let sources = root.join("src/demo");
    std::fs::create_dir_all(&sources).unwrap();
    let file = sources.join("Main.java");
    std::fs::write(&file, PROGRAM).unwrap();
    let compiled = Command::new("javac")
        .args(["-g", "-d"])
        .arg(root.join("classes"))
        .arg(&file)
        .status()
        .ok()?;
    assert!(compiled.success(), "javac failed");
    Some((root, file))
}

fn line_of(text: &str) -> u32 {
    PROGRAM
        .lines()
        .position(|line| line.contains(text))
        .unwrap() as u32
        + 1
}

fn top_line(session: &mut Session, stop: &Stop) -> (String, Option<i32>, Option<PathBuf>) {
    let thread = stop.thread().unwrap();
    let frame = session.stack(thread).unwrap().remove(0);
    (frame.method, frame.line, frame.path)
}

fn value_of(shown: &[debugger::Shown], name: &str) -> String {
    shown
        .iter()
        .find(|variable| variable.name == name)
        .unwrap_or_else(|| panic!("no {name} in {shown:?}"))
        .value
        .clone()
}

#[test]
fn breakpoints_variables_steps_and_an_uncaught_exception() {
    let Some((root, file)) = compiled("cycle") else {
        eprintln!("no javac: skipped");
        return;
    };
    let launch = Launch::Java {
        arguments: vec![
            "-cp".to_owned(),
            root.join("classes").display().to_string(),
            "demo.Main".to_owned(),
        ],
        directory: root.clone(),
    };
    let mut session = Session::start(&launch, &[root.join("src")]).unwrap();
    // Nothing is loaded yet: both wait for their classes.
    let in_main = session
        .set_breakpoint(&file, line_of("int total = order.total()"))
        .unwrap();
    let in_loop = session
        .set_breakpoint(&file, line_of("sum += item"))
        .unwrap();
    assert!(!in_main.verified && !in_loop.verified);
    session.break_on_exceptions(false, true).unwrap();

    let stop = session.continue_to_stop(PATIENCE).unwrap().unwrap();
    assert_eq!(
        stop,
        Stop::Breakpoint {
            thread: stop.thread().unwrap(),
            breakpoints: vec![in_main.id]
        }
    );
    let (method, line, path) = top_line(&mut session, &stop);
    assert_eq!(
        (method.as_str(), line),
        ("main", Some(line_of("int total = order.total()") as i32))
    );
    assert_eq!(
        path.as_deref(),
        Some(file.as_path()),
        "mapped back to the file"
    );
    let thread = stop.thread().unwrap();
    let variables = session.variables(thread, 0).unwrap();
    assert!(value_of(&variables, "order").starts_with("Main.Order@"));
    assert_eq!(value_of(&variables, "numbers"), "int[3] {1, 2, 3}");
    // A JDK value type shows its `toString()`; `this` only once.
    assert_eq!(value_of(&variables, "price"), "1.50");
    assert!(
        !variables.iter().any(|variable| variable.name == "total"),
        "not live yet"
    );
    let order = variables
        .iter()
        .find(|variable| variable.name == "order")
        .unwrap();
    let fields = session.children(order.children.unwrap()).unwrap();
    assert_eq!(value_of(&fields, "id"), "\"A-1\"");
    assert_eq!(value_of(&fields, "items"), "ArrayList(size = 2)");
    assert_eq!(value_of(&fields, "status"), "Main.Status.ACTIVE");
    assert_eq!(
        session.evaluate(thread, 0, "order.items[1]").unwrap().value,
        "4"
    );
    assert_eq!(
        session.evaluate(thread, 0, "numbers.length").unwrap().value,
        "3"
    );
    assert!(
        session
            .evaluate(thread, 0, "order.price")
            .unwrap_err()
            .contains("no field `price`")
    );

    // Into `total()`: the nested class's breakpoint was set when it loaded.
    let stop = session.continue_to_stop(PATIENCE).unwrap().unwrap();
    assert!(matches!(&stop, Stop::Breakpoint { breakpoints, .. } if breakpoints == &[in_loop.id]));
    assert!(
        session
            .breakpoints()
            .iter()
            .all(|breakpoint| breakpoint.verified)
    );
    let (method, line, _) = top_line(&mut session, &stop);
    assert_eq!(
        (method.as_str(), line),
        ("total", Some(line_of("sum += item") as i32))
    );
    let variables = session.variables(stop.thread().unwrap(), 0).unwrap();
    assert_eq!(value_of(&variables, "sum"), "0");
    assert_eq!(value_of(&variables, "item"), "3");
    assert!(value_of(&variables, "this").starts_with("Main.Order@"));
    let stack = session.stack(stop.thread().unwrap()).unwrap();
    assert_eq!(stack[1].method, "main");

    // Out of `total()`, then over to the next line.
    assert!(session.remove_breakpoint(in_loop.id));
    let stop = session
        .step(thread, StepDepth::Out, PATIENCE)
        .unwrap()
        .unwrap();
    assert!(matches!(stop, Stop::Step { .. }));
    assert_eq!(top_line(&mut session, &stop).0, "main");
    let stop = session
        .step(thread, StepDepth::Over, PATIENCE)
        .unwrap()
        .unwrap();
    assert_eq!(
        top_line(&mut session, &stop).1,
        Some(line_of("System.out.println") as i32)
    );
    let variables = session.variables(thread, 0).unwrap();
    assert_eq!(value_of(&variables, "total"), "7");

    // The uncaught exception, then the end.
    let stop = session.continue_to_stop(PATIENCE).unwrap().unwrap();
    assert_eq!(
        stop,
        Stop::Exception {
            thread,
            exception: "IllegalStateException: too much: 7".to_owned(),
            caught: false
        }
    );
    let stop = session.continue_to_stop(PATIENCE).unwrap().unwrap();
    assert_eq!(stop, Stop::Ended { exit_code: Some(1) });
    assert!(session.output_tail(10).iter().any(|line| line == "total 7"));
    session.terminate();
    let _ = std::fs::remove_dir_all(Path::new(&root));
}
