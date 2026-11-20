#!/usr/bin/env python3
"""Tests of gradle_task.py, and of Zed's tasks.json that embeds it.

    python3 -I editors/gradle/test_gradle_task.py

Each test builds a Gradle-shaped directory whose gradlew records how it was
called, instead of running Gradle.
"""

from __future__ import annotations

import io
import json
import os
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
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

    def run_task(
        self, *arguments: str, answer: str = "", path: str | None = None, extra: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess:
        return self.run_command([sys.executable, "-I", str(SCRIPT), *arguments], answer, path, extra)

    def run_command(
        self,
        command: list[str],
        answer: str = "",
        path: str | None = None,
        extra: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess:
        environment = {"GRADLE_CALL": str(self.call_file), "PATH": path or os.defpath, **(extra or {})}
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
        # A profiled task runs the same, the profiler's init script added.
        expected.update({f"profile {key}": arguments for key, arguments in expected.items() if key.startswith("kotlin-")})
        expected["profile-run"] = [":services:payments:run"]
        profiling = FakeProfiling(Path(self.scratch.name))
        self.write_executable(self.root / "gradlew", PROFILED_GRADLE)

        def key_of(task: dict) -> str:
            key = task.get("tags", [shlex.split(task["command"])[5]])[0]
            is_profiled_test = task["label"].startswith("profile: gradle test")
            return f"profile {key}" if is_profiled_test else key

        keys = [key_of(task) for task in tasks]
        self.assertEqual(sorted(keys), sorted(expected))
        for task, key in zip(tasks, keys):
            command = zed_expanded(task["command"], self.variables(key.removeprefix("profile ")))
            # The python3 Zed's shell finds is this test's.
            command = command.replace("'python3'", shlex.quote(sys.executable), 1)
            result = self.run_command(["/bin/sh", "-c", command], answer="2\n", extra=profiling.environment)
            self.assertEqual(result.returncode, 0, result.stderr)
            where, arguments = self.gradle_call()
            self.assertEqual(where, self.root, task["label"])
            if key.startswith("profile"):
                self.assertEqual(arguments[:-3], expected[key], task["label"])
                self.assertEqual(arguments[-3:-1], ["--console=plain", "-I"], task["label"])
            else:
                self.assertEqual(arguments, expected[key], task["label"])

# A gradlew that records its call and, like a JVM the profiler's agent ran
# in, writes a recording where the init script's agent says.
PROFILED_GRADLE = """#!/bin/sh
{ pwd; for argument in "$@"; do echo "$argument"; done; } > "$GRADLE_CALL"
previous=""
for argument in "$@"; do
  if [ "$previous" = "-I" ]; then
    cp "$argument" "$GRADLE_CALL.init"
    recording=$(sed -n "s/.*file=\\([^']*\\)'.*/\\1/p" "$argument" | sed 's/%p/4242/')
    [ -n "$recording" ] && echo "JFR" > "$recording"
  fi
  previous="$argument"
done
exit 0
"""


class FakeProfiling:
    """An async-profiler whose jfrconv writes a page and records its call, a
    browser that records what it opens, and a cache of their own."""

    def __init__(self, scratch: Path) -> None:
        self.home = scratch / "async-profiler"
        (self.home / "lib").mkdir(parents=True, exist_ok=True)
        (self.home / "lib" / "libasyncProfiler.dylib").write_text("")
        self.converted = scratch / "jfrconv-calls"
        self.opened = scratch / "opened"
        BuildFixture.write_executable(
            self.home / "bin" / "jfrconv",
            '#!/bin/sh\necho "$@" >> "$JFRCONV_CALLS"\nfor last in "$@"; do :; done\necho page > "$last"\n',
        )
        browser = scratch / "browser"
        BuildFixture.write_executable(browser, '#!/bin/sh\necho "$1" >> "$OPENED"\n')
        self.cache = scratch / "cache"
        self.environment = {
            "ASYNC_PROFILER_HOME": str(self.home),
            "JFRCONV_CALLS": str(self.converted),
            "OPENED": str(self.opened),
            "KOTLIN_ANALYZER_OPEN_WITH": str(browser),
            "XDG_CACHE_HOME": str(self.cache),
            "HOME": str(scratch / "home"),
        }


class ProfileTest(BuildFixture):
    def setUp(self) -> None:
        super().setUp()
        self.write_executable(self.root / "gradlew", PROFILED_GRADLE)
        self.profiling = FakeProfiling(Path(self.scratch.name))

    def test_a_profiled_test_runs_with_the_agent_and_its_flame_graph_opens(self) -> None:
        result = self.run_task(
            "profile-tests", str(self.source), "pay.PayTest", extra=self.profiling.environment
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        where, arguments = self.gradle_call()
        self.assertEqual(arguments[:6], ["-p", "services/payments", "test", "--tests", "pay.PayTest", "--rerun"])
        init_script = Path(f"{self.call_file}.init").read_text()
        library = self.profiling.home / "lib" / "libasyncProfiler.dylib"
        profiles = self.root / "build" / "kotlin-analyzer" / "profiles"
        self.assertIn(f"['-agentpath:{library}=start,event=cpu,file={profiles}/", init_script)
        self.assertIn("tasks.withType(Test)", init_script)
        self.assertIn("tasks.withType(JavaExec)", init_script)
        (run,) = list(profiles.iterdir())
        self.assertTrue(run.name.endswith("-cpu"), run.name)
        converted = self.profiling.converted.read_text().split()
        self.assertEqual(converted[0], "--cpu")
        self.assertEqual(converted[-2:], [str(run / "profile-4242.jfr"), str(run / "cpu.html")])
        self.assertEqual(self.profiling.opened.read_text().splitlines(), [str(run / "cpu.html")])
        remembered = json.loads((self.profiling.cache / "kotlin-analyzer" / "profiler.json").read_text())
        self.assertEqual(remembered, {"event": "cpu", "options": ""})

    def test_the_last_answers_are_used_without_a_terminal_to_ask_in(self) -> None:
        cache = self.profiling.cache / "kotlin-analyzer"
        cache.mkdir(parents=True)
        (cache / "profiler.json").write_text(json.dumps({"event": "all", "options": "interval=1ms"}))
        result = self.run_task("profile-tests", str(self.source), "pay.PayTest", extra=self.profiling.environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        init_script = Path(f"{self.call_file}.init").read_text()
        self.assertIn("=start,event=cpu,alloc,lock,interval=1ms,file=", init_script)
        kinds = [line.split()[0] for line in self.profiling.converted.read_text().splitlines()]
        self.assertEqual(kinds, ["--cpu", "--alloc", "--lock"])
        self.assertEqual(len(self.profiling.opened.read_text().splitlines()), 3)

    def test_no_recording_is_said_and_fails(self) -> None:
        self.write_executable(self.root / "gradlew", RECORDING_GRADLE)
        result = self.run_task("profile-tests", str(self.source), "pay.PayTest", extra=self.profiling.environment)
        self.assertEqual(result.returncode, 1)
        self.assertIn("No JVM was profiled", result.stderr)
        self.assertFalse(self.profiling.opened.exists())


class ProfileSettingsTest(unittest.TestCase):
    def setUp(self) -> None:
        sys.path.insert(0, str(HERE))
        import gradle_task

        self.gradle_task = gradle_task

    def asked(self, answers: list[str], last=None):
        replies = iter(answers)
        last = last or self.gradle_task.DEFAULT_SETTINGS
        with open(os.devnull, "w") as quiet:
            stdout = sys.stdout
            sys.stdout = quiet
            try:
                return self.gradle_task.asked_settings(last, lambda _prompt: next(replies))
            finally:
                sys.stdout = stdout

    def test_events_by_number_or_name_and_enter_keeps_the_last(self) -> None:
        settings = self.gradle_task.ProfileSettings
        self.assertEqual(self.asked(["2", ""]), settings("alloc", ""))
        self.assertEqual(self.asked(["wall", "interval=5ms, threads"]), settings("wall", "interval=5ms, threads"))
        last = settings("lock", "threads")
        self.assertEqual(self.asked(["", ""], last), last)
        self.assertEqual(self.asked(["", "-"], last), settings("lock", ""))
        self.assertEqual(
            settings("all", "interval=5ms, threads").agent_options(),
            ["event=cpu,alloc,lock", "interval=5ms", "threads"],
        )

    def test_an_unknown_event_or_a_file_option_is_refused(self) -> None:
        failed = self.gradle_task.ProfilingFailed
        with self.assertRaises(failed):
            self.asked(["7", ""])
        with self.assertRaises(failed):
            self.asked(["cpu", "interval=1ms,file=/tmp/x.jfr"])


class ProfilerReleaseTest(unittest.TestCase):
    def setUp(self) -> None:
        sys.path.insert(0, str(HERE))
        import gradle_task

        self.gradle_task = gradle_task
        self.scratch = tempfile.TemporaryDirectory()
        self.directory = Path(self.scratch.name).resolve()

    def tearDown(self) -> None:
        self.scratch.cleanup()

    RELEASE = {
        "tag_name": "v4.6",
        "assets": [
            {
                "name": f"async-profiler-4.6-{suffix}",
                "browser_download_url": f"https://github.com/async-profiler/async-profiler/releases/download/v4.6/async-profiler-4.6-{suffix}",
                "digest": f"sha256:{index:064x}",
            }
            for index, suffix in enumerate(
                ["linux-arm64-debug.tar.gz", "linux-arm64.tar.gz", "linux-x64.tar.gz", "macos.zip"]
            )
        ],
    }

    def test_the_asset_for_the_platform_without_debug_symbols(self) -> None:
        asset = self.gradle_task.release_asset
        self.assertEqual(asset(self.RELEASE, "Darwin", "arm64")[0], "async-profiler-4.6-macos.zip")
        self.assertEqual(asset(self.RELEASE, "Linux", "aarch64")[0], "async-profiler-4.6-linux-arm64.tar.gz")
        name, url, sha256 = asset(self.RELEASE, "Linux", "x86_64")
        self.assertEqual(name, "async-profiler-4.6-linux-x64.tar.gz")
        self.assertTrue(url.startswith("https://github.com/"))
        self.assertEqual(sha256, f"{2:064x}")
        with self.assertRaises(self.gradle_task.ProfilingFailed):
            asset(self.RELEASE, "Windows", "AMD64")
        without_digest = {"assets": [{**self.RELEASE["assets"][3], "digest": None}]}
        with self.assertRaises(self.gradle_task.ProfilingFailed):
            asset(without_digest, "Darwin", "arm64")

    def test_unpacking_keeps_modes_and_refuses_what_leaves_the_directory(self) -> None:
        archive = self.directory / "a.zip"
        with zipfile.ZipFile(archive, "w") as written:
            info = zipfile.ZipInfo("async-profiler-4.6-macos/bin/jfrconv")
            info.external_attr = 0o755 << 16
            written.writestr(info, "#!/bin/sh\n")
            written.writestr("async-profiler-4.6-macos/lib/libasyncProfiler.dylib", "")
        self.gradle_task.unpack(archive, self.directory / "zip")
        converter = self.directory / "zip/async-profiler-4.6-macos/bin/jfrconv"
        self.assertTrue(os.access(converter, os.X_OK))

        escaping = self.directory / "b.tar.gz"
        with tarfile.open(escaping, "w:gz") as written:
            info = tarfile.TarInfo("../outside")
            written.addfile(info, io.BytesIO(b""))
        with self.assertRaises(self.gradle_task.ProfilingFailed):
            self.gradle_task.unpack(escaping, self.directory / "tar")
        self.assertFalse((self.directory / "outside").exists())

        linked = self.directory / "c.tar.gz"
        with tarfile.open(linked, "w:gz") as written:
            info = tarfile.TarInfo("profiler/lib")
            info.type = tarfile.SYMTYPE
            info.linkname = "/etc"
            written.addfile(info)
        with self.assertRaises(self.gradle_task.ProfilingFailed):
            self.gradle_task.unpack(linked, self.directory / "linked")

    def test_the_profiler_comes_from_the_environment_the_path_or_the_newest_download(self) -> None:
        def fake_profiler(home: Path) -> Path:
            (home / "lib").mkdir(parents=True)
            (home / "lib" / "libasyncProfiler.so").write_text("")
            return home

        find = self.gradle_task.find_profiler
        cache = self.directory / "cache"
        fake_profiler(cache / "async-profiler" / "v3.0")
        newest = fake_profiler(cache / "async-profiler" / "v4.10")
        fake_profiler(cache / "async-profiler" / "v4.9")
        self.assertEqual(find({}, cache).home, newest)
        brewed = fake_profiler(self.directory / "brew")
        BuildFixture.write_executable(brewed / "bin" / "asprof", "")
        self.assertEqual(find({"PATH": str(brewed / "bin")}, cache).home, brewed)
        chosen = fake_profiler(self.directory / "chosen")
        self.assertEqual(find({"ASYNC_PROFILER_HOME": str(chosen), "PATH": str(brewed / "bin")}, cache).home, chosen)
        self.assertIsNone(find({}, self.directory / "empty-cache"))

    def test_java_comes_from_jdks_only_when_nothing_else_runs_it(self) -> None:
        def fake_jdk(home: Path, version: str) -> Path:
            BuildFixture.write_executable(home / "bin" / "java", "")
            (home / "release").write_text(f'JAVA_VERSION="{version}"\n')
            return home

        user = self.directory / "user"
        fake_jdk(user / ".jdks" / "temurin-17.0.9", "17.0.9")
        newest = fake_jdk(user / ".jdks" / "zulu-21.0.5" / "Contents" / "Home", "21.0.5")
        nothing = {"HOME": str(user), "PATH": str(self.directory / "empty")}
        self.assertEqual(self.gradle_task.with_java(nothing)["JAVA_HOME"], str(newest))
        java_home = fake_jdk(self.directory / "jdk", "11.0.2")
        given = {**nothing, "JAVA_HOME": str(java_home)}
        self.assertEqual(self.gradle_task.with_java(given), given)
        self.assertEqual(self.gradle_task.with_java({"PATH": ""}), {"PATH": ""})


if __name__ == "__main__":
    unittest.main()
