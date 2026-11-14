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


def tasks_json() -> str:
    script = SCRIPT.read_text()
    tasks = [
        {
            "label": label,
            "command": "python3",
            "args": ["-I", "-c", script, task, "$ZED_FILE"],
            "use_new_terminal": False,
            "allow_concurrent_runs": False,
            "reveal": "always",
        }
        for label, task in TASKS
    ]
    return json.dumps(tasks, indent=2) + "\n"


if __name__ == "__main__":
    TASKS_FILE.write_text(tasks_json())
    print(f"wrote {TASKS_FILE}", file=sys.stderr)
