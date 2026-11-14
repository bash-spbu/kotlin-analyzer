#!/usr/bin/env python3
"""Tests of gradle_task.py, and of Zed's tasks.json that embeds it.

    python3 -I editors/gradle/test_gradle_task.py

Each test builds a Gradle-shaped directory whose gradlew records how it was
called, instead of running Gradle.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "gradle_task.py"
sys.path.insert(0, str(HERE))
import zed_tasks  # noqa: E402

RECORDING_GRADLE = """#!/bin/sh
{ pwd; for argument in "$@"; do echo "$argument"; done; } > "$GRADLE_CALL"
"""


class BuildFixture(unittest.TestCase):
    """A build with :app, :services and :services:payments, and a recording gradlew."""

    def setUp(self) -> None:
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name).resolve() / "build"
        self.call_file = Path(self.scratch.name) / "gradle-call"
        for script in [
            "settings.gradle.kts",
            "app/build.gradle.kts",
            "services/build.gradle.kts",
            "services/payments/build.gradle",
        ]:
            (self.root / script).parent.mkdir(parents=True, exist_ok=True)
            (self.root / script).write_text("")
        self.source = self.root / "services/payments/src/main/kotlin/pay/Pay.kt"
        self.source.parent.mkdir(parents=True)
        self.source.write_text("package pay\n")
        self.write_executable(self.root / "gradlew", RECORDING_GRADLE)

    def tearDown(self) -> None:
        self.scratch.cleanup()

    @staticmethod
    def write_executable(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o755)

    def run_task(self, *arguments: str, answer: str = "", path: str | None = None) -> subprocess.CompletedProcess:
        return self.run_command([sys.executable, "-I", str(SCRIPT), *arguments], answer, path)

    def run_command(self, command: list[str], answer: str = "", path: str | None = None) -> subprocess.CompletedProcess:
        environment = {"GRADLE_CALL": str(self.call_file), "PATH": path or os.defpath}
        return subprocess.run(
            command, input=answer, capture_output=True, text=True, env=environment, timeout=30
        )

    def gradle_call(self) -> tuple[Path, list[str]]:
        """Where gradle ran and its arguments."""
        lines = self.call_file.read_text().splitlines()
        return Path(lines[0]).resolve(), lines[1:]


class GradleTaskTest(BuildFixture):
    def test_projects_are_the_ancestors_with_build_scripts_up_to_the_settings(self) -> None:
        result = self.run_task("projects", str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            [
                {"path": ":services:payments", "directory": str(self.root / "services/payments")},
                {"path": ":services", "directory": str(self.root / "services")},
                {"path": ":", "directory": str(self.root)},
            ],
        )

    def test_build_and_clean_run_in_the_root_whatever_the_file(self) -> None:
        result = self.run_task("build", str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.gradle_call(), (self.root, ["build", "-x", "test"]))
        self.assertIn("> ./gradlew build -x test", result.stdout)
        self.run_task("clean", str(self.root / "app/build.gradle.kts"))
        self.assertEqual(self.gradle_call(), (self.root, ["clean"]))

    def test_test_runs_in_the_nearest_project_with_its_subprojects(self) -> None:
        result = self.run_task("test", str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.gradle_call(),
            (self.root, ["-p", "services/payments", "test", "--rerun-tasks"]),
        )
        self.assertIn("Testing :services:payments", result.stdout)

    def test_a_parents_build_script_tests_the_parent_and_the_root_tests_everything(self) -> None:
        self.run_task("test", str(self.root / "services/build.gradle.kts"))
        self.assertEqual(self.gradle_call(), (self.root, ["-p", "services", "test", "--rerun-tasks"]))
        self.run_task("test", str(self.root / "settings.gradle.kts"))
        self.assertEqual(self.gradle_call(), (self.root, ["test", "--rerun-tasks"]))

    def test_choose_offers_the_project_and_its_parents(self) -> None:
        result = self.run_task("choose", str(self.source), answer="2\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1) :services:payments", result.stdout)
        self.assertIn("3) :", result.stdout)
        self.assertEqual(self.gradle_call(), (self.root, ["-p", "services", "test", "--rerun-tasks"]))
        self.run_task("choose", str(self.source), answer="\n")
        self.assertEqual(
            self.gradle_call(),
            (self.root, ["-p", "services/payments", "test", "--rerun-tasks"]),
        )
        refused = self.run_task("choose", str(self.source), answer="9\n")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("no project 9", refused.stderr)

    def test_without_a_wrapper_gradle_comes_from_the_path(self) -> None:
        (self.root / "gradlew").unlink()
        bin_directory = Path(self.scratch.name) / "bin"
        self.write_executable(bin_directory / "gradle", RECORDING_GRADLE)
        result = self.run_task("clean", str(self.source), path=f"{bin_directory}:{os.defpath}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("> gradle clean", result.stdout)
        self.assertEqual(self.gradle_call(), (self.root, ["clean"]))
        missing = self.run_task("clean", str(self.source))
        self.assertEqual(missing.returncode, 1)
        self.assertIn("gradle isn't on the PATH", missing.stderr)

    def test_outside_a_build_nothing_runs(self) -> None:
        outside = Path(self.scratch.name) / "elsewhere/Loose.kt"
        outside.parent.mkdir()
        outside.write_text("")
        result = self.run_task("build", str(outside))
        self.assertEqual(result.returncode, 1)
        self.assertIn("isn't in a Gradle build", result.stderr)
        self.assertFalse(self.call_file.exists())


class ZedTasksTest(unittest.TestCase):
    def test_tasks_json_embeds_the_current_script(self) -> None:
        self.assertEqual(
            zed_tasks.TASKS_FILE.read_text(),
            zed_tasks.tasks_json(),
            "tasks.json is stale: run python3 editors/gradle/zed_tasks.py",
        )

    def test_zed_substitutes_nothing_in_the_script_but_its_file_variable(self) -> None:
        # Zed expands $NAME and ${NAME} in a task's arguments; the script
        # must have none of its own.
        self.assertIsNone(re.search(r"\$\{?[A-Za-z_]", SCRIPT.read_text()))


class ZedTaskRunTest(BuildFixture):
    def test_each_zed_task_runs_like_the_script(self) -> None:
        tasks = json.loads(zed_tasks.TASKS_FILE.read_text())
        expected = {
            "build": ["build", "-x", "test"],
            "clean": ["clean"],
            "test": ["-p", "services/payments", "test", "--rerun-tasks"],
            "choose": ["-p", "services", "test", "--rerun-tasks"],
        }
        self.assertEqual([task["args"][3] for task in tasks], list(expected))
        for task in tasks:
            arguments = [argument.replace("$ZED_FILE", str(self.source)) for argument in task["args"]]
            command = [sys.executable if task["command"] == "python3" else task["command"], *arguments]
            result = self.run_command(command, answer="2\n")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.gradle_call(), (self.root, expected[task["args"][3]]), task["label"])


if __name__ == "__main__":
    unittest.main()
