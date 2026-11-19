#!/usr/bin/env python3
"""Writes the Zed extension's Kotlin tasks (editors/zed/languages/kotlin/tasks.json).

Zed tasks can't reach a file shipped with an extension, so each task runs
gradle_task.py's text with `python3 -I -c`. Zed expands variables in a
task's arguments and hands them to the user's shell, so the script goes in
base64 — nothing in it for either to read — and the test runs take their
test's names from the environment, not from arguments. Run this after
changing gradle_task.py; test_gradle_task.py fails while the two disagree.
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
# shows). The label names only the captures its runnable has, so Zed offers
# the task only there; the script reads them from the environment.
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

# Decodes and runs the script given as the next argument.
BOOTSTRAP = "import base64,sys;exec(base64.b64decode(sys.argv.pop(1)))"


def task(label: str, arguments: list[str]) -> dict:
    encoded = base64.b64encode(SCRIPT.read_bytes()).decode("ascii")
    return {
        "label": label,
        "command": "python3",
        "args": ["-I", "-c", BOOTSTRAP, encoded, *arguments],
        "use_new_terminal": False,
        "allow_concurrent_runs": False,
        "reveal": "always",
    }


def tasks_json() -> str:
    tasks = [task(label, [name, "$ZED_FILE"]) for label, name in TASKS]
    for tag, shown in TEST_RUNS:
        run = task(f"gradle test --tests {shown}", ["zed-tests", "$ZED_FILE"])
        run["tags"] = [tag]
        tasks.append(run)
    return json.dumps(tasks, indent=2) + "\n"


if __name__ == "__main__":
    TASKS_FILE.write_text(tasks_json())
    print(f"wrote {TASKS_FILE}", file=sys.stderr)
