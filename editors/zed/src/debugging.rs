//! Zed's debugger for Kotlin: the `Kotlin` adapter is the server's
//! `kotlin-analyzer dap`, and the `kotlin-gradle` locator turns the test
//! tasks of the gutter's runnables (`tasks.json`) into debug scenarios.
//! Configurations are the adapter's (`debug_adapter_schemas/Kotlin.json`).

use zed_extension_api::serde_json::{Value, json};

/// The adapter's name: `adapter` in `debug.json`, `debuggers` in the
/// language's config.
pub const ADAPTER: &str = "Kotlin";

/// The locator's name in `extension.toml`.
pub const LOCATOR: &str = "kotlin-gradle";

/// What a configuration asks of the adapter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RequestKind {
    Launch,
    Attach,
}

/// The kind a configuration's `request` names; an error when it names
/// none, as Zed asks.
pub fn request_kind(configuration: &Value) -> Result<RequestKind, String> {
    match configuration.get("request").and_then(Value::as_str) {
        Some("launch") => Ok(RequestKind::Launch),
        Some("attach") => Ok(RequestKind::Attach),
        Some(other) => Err(format!("`request` is `launch` or `attach`, not `{other}`")),
        None => Err("the configuration has no `request`".to_owned()),
    }
}

/// The configuration of a program started from Zed's "new session" form:
/// `java …` or Gradle (`./gradlew bootRun`).
pub fn program_configuration(
    program: &str,
    arguments: &[String],
    directory: Option<&str>,
    environment: &[(String, String)],
) -> Value {
    let mut configuration = json!({
        "request": "launch",
        "program": program,
        "args": arguments,
    });
    if let Some(directory) = directory {
        configuration["cwd"] = json!(directory);
    }
    if !environment.is_empty() {
        let environment: zed_extension_api::serde_json::Map<String, Value> = environment
            .iter()
            .map(|(name, value)| (name.clone(), json!(value)))
            .collect();
        configuration["env"] = Value::Object(environment);
    }
    configuration
}

/// The configuration debugging a test task's test: the task's `zed-tests`
/// arguments — the file, package, outer class, class and method, still
/// Zed's variables, which Zed fills in as it does the task's. `None` for
/// any other task.
pub fn test_configuration(task_command: &str) -> Option<Value> {
    let words = single_quoted_words(task_command);
    let start = words.iter().position(|word| word == "zed-tests")? + 1;
    let [file, package, outer_class, class_name, method] = words.get(start..start + 5)? else {
        return None;
    };
    Some(json!({
        "request": "launch",
        "test": {
            "file": file,
            "package": package,
            "outerClass": outer_class,
            "class": class_name,
            "method": method,
        },
    }))
}

/// The words of a command whose every word is single-quoted, as the
/// generated tasks' are.
fn single_quoted_words(command: &str) -> Vec<String> {
    command
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_tasks_arguments_become_the_tests_configuration() {
        let command = "'python3' '-I' '-c' 'import base64,sys;exec(base64.b64decode(sys.argv.pop(1)))' \
            'IyEv' 'zed-tests' '$ZED_FILE' '${ZED_CUSTOM_kotlin_package_name:}' \
            '${ZED_CUSTOM_kotlin_outer_class_name:}' '$ZED_CUSTOM_kotlin_class_name' \
            '${ZED_CUSTOM_kotlin_method_name:}'";
        assert_eq!(
            test_configuration(command),
            Some(json!({
                "request": "launch",
                "test": {
                    "file": "$ZED_FILE",
                    "package": "${ZED_CUSTOM_kotlin_package_name:}",
                    "outerClass": "${ZED_CUSTOM_kotlin_outer_class_name:}",
                    "class": "$ZED_CUSTOM_kotlin_class_name",
                    "method": "${ZED_CUSTOM_kotlin_method_name:}",
                },
            }))
        );
    }

    #[test]
    fn other_tasks_have_no_test() {
        assert_eq!(test_configuration("'python3' '-I' 'build' '$ZED_FILE'"), None);
        assert_eq!(test_configuration("cargo test"), None);
        assert_eq!(test_configuration("'zed-tests' '$ZED_FILE'"), None, "pieces missing");
    }

    #[test]
    fn the_shipped_test_tasks_all_debug() {
        let tasks: Value =
            zed_extension_api::serde_json::from_str(include_str!("../languages/kotlin/tasks.json"))
                .unwrap();
        let tasks = tasks.as_array().unwrap();
        let test_tasks: Vec<&Value> = tasks.iter().filter(|task| task.get("tags").is_some()).collect();
        assert!(!test_tasks.is_empty());
        for task in test_tasks {
            assert!(
                test_configuration(task["command"].as_str().unwrap()).is_some(),
                "{}",
                task["label"]
            );
        }
    }

    #[test]
    fn a_programs_configuration_and_request_kinds() {
        let configuration = program_configuration(
            "./gradlew",
            &["bootRun".to_owned()],
            Some("/build"),
            &[("SPRING_PROFILES_ACTIVE".to_owned(), "local".to_owned())],
        );
        assert_eq!(
            configuration,
            json!({
                "request": "launch",
                "program": "./gradlew",
                "args": ["bootRun"],
                "cwd": "/build",
                "env": { "SPRING_PROFILES_ACTIVE": "local" },
            })
        );
        assert_eq!(request_kind(&configuration), Ok(RequestKind::Launch));
        assert_eq!(
            request_kind(&json!({ "request": "attach", "port": 5005 })),
            Ok(RequestKind::Attach)
        );
        assert!(request_kind(&json!({ "program": "java" })).is_err());
    }
}
