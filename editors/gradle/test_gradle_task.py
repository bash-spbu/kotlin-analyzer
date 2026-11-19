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
import shlex
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

    def test_tests_run_the_filter_in_the_nearest_project_only(self) -> None:
        test_file = self.root / "services/payments/src/test/kotlin/pay/PayTest.kt"
        test_file.parent.mkdir(parents=True)
        test_file.write_text("package pay\n")
        result = self.run_task("tests", str(test_file), "pay.PayTest.`charges in full`")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Testing pay.PayTest.charges in full in :services:payments", result.stdout)
        self.assertEqual(
            self.gradle_call(),
            (
                self.root,
                ["-p", "services/payments", "test", "--tests", "pay.PayTest.charges in full", "--rerun"],
            ),
        )
        # A class in no package, nested.
        self.run_task("tests", str(self.root / "settings.gradle.kts"), ".Outer$Nested.works")
        self.assertEqual(
            self.gradle_call(),
            (self.root, ["test", "--tests", "Outer$Nested.works", "--rerun"]),
        )
        missing_filter = self.run_task("tests", str(test_file))
        self.assertEqual(missing_filter.returncode, 2)

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


def zed_expanded(template: str, variables: dict[str, str]) -> str:
    """`template` as Zed expands its variables: `$NAME`, `${NAME}`, and
    `${NAME:default}` (the default when Zed has no such variable)."""

    def value(match: re.Match) -> str:
        name = match.group(1) or match.group(3)
        default = match.group(2)
        if name in variables:
            return variables[name]
        if default is not None:
            return default[1:]
        raise KeyError(f"Zed has no {name}: it offers no such task")

    return re.sub(r"\$\{(ZED_\w+)(:[^}]*)?\}|\$(ZED_\w+)", value, template)


class ZedTestFilterTest(unittest.TestCase):
    def test_a_runnable_without_a_package_an_outer_class_or_a_method(self) -> None:
        sys.path.insert(0, str(HERE))
        import gradle_task

        self.assertEqual(gradle_task.zed_test_filter("", "", "Loose", ""), "Loose")
        self.assertEqual(
            gradle_task.zed_test_filter("app", "Outer", "Nested", "`works fine`"),
            "app.Outer$Nested.works fine",
        )


class ZedTasksTest(unittest.TestCase):
    def test_tasks_json_embeds_the_current_script(self) -> None:
        self.assertEqual(
            zed_tasks.TASKS_FILE.read_text(),
            zed_tasks.tasks_json(),
            "tasks.json is stale: run python3 editors/gradle/zed_tasks.py",
        )

    def test_each_task_is_one_shell_command_whose_words_zed_cannot_split(self) -> None:
        # Zed puts a task's `args` into the shell's command line unquoted;
        # a lone command is shell code as written.
        tricky = {
            "ZED_FILE": "/work/My Project/src/$HOME/Pay;Test.kt",
            "ZED_CUSTOM_kotlin_package_name": "pay",
            "ZED_CUSTOM_kotlin_class_name": "PayTest",
            "ZED_CUSTOM_kotlin_method_name": "`charges (in full); twice`",
        }
        for task in json.loads(zed_tasks.TASKS_FILE.read_text()):
            self.assertNotIn("args", task)
            self.assertFalse(task["show_command"], "the base64 script isn't shown")
            self.assertEqual(task["shell"], {"with_arguments": {"program": "/bin/sh", "args": ["-c"]}})
            command = zed_expanded(task["command"], tricky)
            self.assertNotIn("\n", command)
            words = shlex.split(command)
            self.assertEqual(words[:4], ["python3", "-I", "-c", zed_tasks.BOOTSTRAP], task["label"])
            self.assertIn(tricky["ZED_FILE"], words, task["label"])


class ZedTaskRunTest(BuildFixture):
    # What Zed knows when it runs each task: the file, and what a runnable
    # in `pay.PayTest.Refunds` captured, by tag.
    CAPTURES = {
        "kotlin-test-method": {"class": "Refunds", "method": "`refunds in full`"},
        "kotlin-test-nested-method": {
            "outer_class": "PayTest",
            "class": "Refunds",
            "method": "`refunds in full`",
        },
        "kotlin-test-class": {"class": "Refunds"},
        "kotlin-test-nested-class": {"outer_class": "PayTest", "class": "Refunds"},
    }

    def variables(self, key: str) -> dict[str, str]:
        variables = {"ZED_FILE": str(self.source)}
        if key in self.CAPTURES:
            captures = {"package": "pay", **self.CAPTURES[key]}
            variables.update({f"ZED_CUSTOM_kotlin_{name}_name": value for name, value in captures.items()})
        return variables

    def test_each_zed_task_runs_like_the_script(self) -> None:
        tasks = json.loads(zed_tasks.TASKS_FILE.read_text())
        payments = ["-p", "services/payments", "test", "--tests"]
        expected = {
            "build": ["build", "-x", "test"],
            "clean": ["clean"],
            "test": ["-p", "services/payments", "test", "--rerun-tasks"],
            "choose": ["-p", "services", "test", "--rerun-tasks"],
            "kotlin-test-method": [*payments, "pay.Refunds.refunds in full", "--rerun"],
            "kotlin-test-nested-method": [*payments, "pay.PayTest$Refunds.refunds in full", "--rerun"],
            "kotlin-test-class": [*payments, "pay.Refunds", "--rerun"],
            "kotlin-test-nested-class": [*payments, "pay.PayTest$Refunds", "--rerun"],
        }
        keys = [task.get("tags", [shlex.split(task["command"])[5]])[0] for task in tasks]
        self.assertEqual(keys, list(expected))
        for task, key in zip(tasks, keys):
            command = zed_expanded(task["command"], self.variables(key))
            # The python3 Zed's shell finds is this test's.
            command = command.replace("'python3'", shlex.quote(sys.executable), 1)
            result = self.run_command(["/bin/sh", "-c", command], answer="2\n")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.gradle_call(), (self.root, expected[key]), task["label"])

if __name__ == "__main__":
    unittest.main()
