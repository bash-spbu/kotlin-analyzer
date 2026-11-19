#!/usr/bin/env python3
"""Writes the Zed extension's Kotlin tasks (editors/zed/languages/kotlin/tasks.json).

Zed tasks can't reach a file shipped with an extension, so each task runs
gradle_task.py's text with `python3 -I -c`. A task is one `/bin/sh -c`
command with no `args`: Zed puts arguments into the shell's command line
unquoted (it splits `a;b` and globs `exec(…)`), but a lone command is
shell code as written. So the quoting is ours: the script goes in base64,
and Zed's variables — the file, a runnable's captures — are expanded by
Zed inside single quotes. Run this after changing gradle_task.py;
test_gradle_task.py fails while the two disagree.
"""

from __future__ import annotations

import base64
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "gradle_task.py"
TASKS_FILE = HERE.parent / "zed" / "languages" / "kotlin" / "tasks.json"

# (label, gradle_task.py's task)
TASKS = [
    ("gradle build -x test", "build"),
    ("gradle clean", "clean"),
    ("gradle test --rerun-tasks: this file's project", "test"),
    ("gradle test --rerun-tasks: choose the project or a parent", "choose"),
]

# The runnables (languages/kotlin/runnables.scm): (tag, what the label
# shows). A label names only the captures its runnable has, so Zed offers
# the task only there; a capture it may lack has an empty default.
TEST_RUNS = [
    ("kotlin-test-method", "$ZED_CUSTOM_kotlin_class_name.$ZED_CUSTOM_kotlin_method_name"),
    (
        "kotlin-test-nested-method",
        "$ZED_CUSTOM_kotlin_outer_class_name.$ZED_CUSTOM_kotlin_class_name"
        ".$ZED_CUSTOM_kotlin_method_name",
    ),
    ("kotlin-test-class", "$ZED_CUSTOM_kotlin_class_name"),
    ("kotlin-test-nested-class", "$ZED_CUSTOM_kotlin_outer_class_name.$ZED_CUSTOM_kotlin_class_name"),
]

# gradle_task.py zed-tests' arguments after the file, as Zed fills them in.
TEST_PIECES = [
    "${ZED_CUSTOM_kotlin_package_name:}",
    "${ZED_CUSTOM_kotlin_outer_class_name:}",
    "$ZED_CUSTOM_kotlin_class_name",
    "${ZED_CUSTOM_kotlin_method_name:}",
]

# Decodes and runs the script given as the next argument.
BOOTSTRAP = "import base64,sys;exec(base64.b64decode(sys.argv.pop(1)))"


def single_quoted(text: str) -> str:
    assert "'" not in text, text
    return f"'{text}'"


def task(label: str, arguments: list[str]) -> dict:
    """A task running gradle_task.py with `arguments`, each single-quoted
    for the shell (Zed expands its variables in them first)."""
    encoded = base64.b64encode(SCRIPT.read_bytes()).decode("ascii")
    words = ["python3", "-I", "-c", BOOTSTRAP, encoded, *arguments]
    return {
        "label": label,
        "command": " ".join(single_quoted(word) for word in words),
        "use_new_terminal": False,
        "allow_concurrent_runs": False,
        "reveal": "always",
        "shell": {"with_arguments": {"program": "/bin/sh", "args": ["-c"]}},
    }


def tasks_json() -> str:
    tasks = [task(label, [name, "$ZED_FILE"]) for label, name in TASKS]
    for tag, shown in TEST_RUNS:
        run = task(f"gradle test --tests {shown}", ["zed-tests", "$ZED_FILE", *TEST_PIECES])
        run["tags"] = [tag]
        tasks.append(run)
    return json.dumps(tasks, indent=2) + "\n"

if __name__ == "__main__":
    TASKS_FILE.write_text(tasks_json())
    print(f"wrote {TASKS_FILE}", file=sys.stderr)
