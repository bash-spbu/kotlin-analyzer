#!/usr/bin/env python3
"""Writes the Zed extension's Kotlin tasks (editors/zed/languages/kotlin/tasks.json).

Zed tasks can't reach a file shipped with an extension, so each task runs
gradle_task.py's text with `python3 -I -c`. Run this after changing
gradle_task.py; test_gradle_task.py fails while the two disagree.
"""

from __future__ import annotations

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

# The runnables (languages/kotlin/runnables.scm): (tag, the test filter in
# its captures). A class in no package has no package capture, and a `/`
# stands for a nested class's `$` (gradle_task.py's gradle_test_filter).
PACKAGE = "${ZED_CUSTOM_kotlin_package_name:}"
TEST_RUNS = [
    (
        "kotlin-test-method",
        PACKAGE + ".$ZED_CUSTOM_kotlin_class_name.$ZED_CUSTOM_kotlin_method_name",
    ),
    (
        "kotlin-test-nested-method",
        PACKAGE
        + ".$ZED_CUSTOM_kotlin_outer_class_name/$ZED_CUSTOM_kotlin_class_name"
        + ".$ZED_CUSTOM_kotlin_method_name",
    ),
    ("kotlin-test-class", PACKAGE + ".$ZED_CUSTOM_kotlin_class_name"),
    (
        "kotlin-test-nested-class",
        PACKAGE + ".$ZED_CUSTOM_kotlin_outer_class_name/$ZED_CUSTOM_kotlin_class_name",
    ),
]


def task(label: str, arguments: list[str]) -> dict:
    return {
        "label": label,
        "command": "python3",
        "args": ["-I", "-c", SCRIPT.read_text(), *arguments],
        "use_new_terminal": False,
        "allow_concurrent_runs": False,
        "reveal": "always",
    }


def tasks_json() -> str:
    tasks = [task(label, [name, "$ZED_FILE"]) for label, name in TASKS]
    for tag, test_filter in TEST_RUNS:
        run = task(f"gradle test --tests {test_filter}", ["tests", "$ZED_FILE", test_filter])
        run["tags"] = [tag]
        tasks.append(run)
    return json.dumps(tasks, indent=2) + "\n"


if __name__ == "__main__":
    TASKS_FILE.write_text(tasks_json())
    print(f"wrote {TASKS_FILE}", file=sys.stderr)
