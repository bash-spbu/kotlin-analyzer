#!/usr/bin/env python3
"""Gradle tasks for the file an editor has open.

Every editor integration of kotlin-analyzer runs this script; Zed embeds it
in its tasks.json (see zed_tasks.py).

    gradle_task.py build <path>     gradle build -x test, for the whole build
    gradle_task.py clean <path>     gradle clean, for the whole build
    gradle_task.py test <path>      gradle test --rerun-tasks in the project
                                    holding <path>, with its subprojects
    gradle_task.py choose <path>    asks which project to test: the one
                                    holding <path> or one of its parents
    gradle_task.py tests <path> <filter>
                                    gradle test --tests <filter> --rerun in
                                    the project holding <path>: a test class
                                    (`app.OrderTest`, `app.Outer$Nested`) or
                                    function (`app.OrderTest.creates order`)
    gradle_task.py zed-tests <path> <package> <outer class> <class> <method>
                                    the same, for the test a Zed runnable
                                    names: the outer class, the method or
                                    the package may be empty
    gradle_task.py projects <path>  prints those projects, nearest first, as
                                    JSON: [{"path": ":app", "directory": …}]

<path> is a file or a directory. The projects holding it are its ancestor
directories with a build script, up to the build's root: the nearest
directory with a settings script. Gradle is the root's gradlew, else
`gradle` on the PATH.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

BUILD_SCRIPTS = ("build.gradle.kts", "build.gradle")
SETTINGS_SCRIPTS = ("settings.gradle.kts", "settings.gradle")
TASKS = ("build", "clean", "test", "choose", "projects", "tests", "zed-tests")


class NotABuild(Exception):
    """The path isn't inside a Gradle build, or there's no Gradle to run."""


@dataclass(frozen=True)
class Build:
    root: Path
    # The project directories holding the path, nearest first; the root last.
    projects: list[Path]
    # The Gradle to run, and how to show it.
    gradle: str
    shown_gradle: str

    def gradle_path(self, project: Path) -> str:
        """The project's Gradle path: ":" for the root, else ":a:b"."""
        below_root = project.relative_to(self.root).parts
        return ":" + ":".join(below_root)

    def test_arguments(self, project: Path) -> list[str]:
        """Gradle's arguments to test `project` and its subprojects."""
        if project == self.root:
            return ["test", "--rerun-tasks"]
        directory = project.relative_to(self.root).as_posix()
        return ["-p", directory, "test", "--rerun-tasks"]


    def filtered_test_arguments(self, project: Path, test_filter: str) -> list[str]:
        """Gradle's arguments to run the tests `test_filter` names in `project`."""
        arguments = ["test", "--tests", test_filter, "--rerun"]
        if project == self.root:
            return arguments
        return ["-p", project.relative_to(self.root).as_posix(), *arguments]


def gradle_test_filter(written: str) -> str:
    """A test filter as Gradle takes it: no backticks around Kotlin names,
    no empty segments (a class in no package: `.OrderTest`)."""
    segments = [segment for segment in written.replace("`", "").split(".") if segment != ""]
    return ".".join(segments)


def zed_test_filter(package: str, outer_class: str, class_name: str, method: str) -> str:
    """The filter for the test a Zed runnable (runnables.scm) captured."""
    classes = f"{outer_class}${class_name}" if outer_class else class_name
    return gradle_test_filter(".".join([package, classes, method]))


def has_any(directory: Path, names: tuple[str, ...]) -> bool:
    return any((directory / name).is_file() for name in names)


def find_build(path: Path) -> Build:
    start = path.resolve()
    if not start.is_dir():
        start = start.parent
    projects: list[Path] = []
    root = None
    for directory in [start, *start.parents]:
        is_project = has_any(directory, BUILD_SCRIPTS) or has_any(directory, SETTINGS_SCRIPTS)
        if is_project:
            projects.append(directory)
        if has_any(directory, SETTINGS_SCRIPTS):
            root = directory
            break
    if root is None and projects:
        # A build without a settings script: its outermost build script.
        root = projects[-1]
    if root is None:
        raise NotABuild(f"{path} isn't in a Gradle build")
    wrapper = root / "gradlew"
    if wrapper.is_file() and os.access(wrapper, os.X_OK):
        return Build(root, projects, str(wrapper), "./gradlew")
    installed = shutil.which("gradle")
    if installed is not None:
        return Build(root, projects, installed, "gradle")
    raise NotABuild(f"{root} has no gradlew and gradle isn't on the PATH")


def run(build: Build, arguments: list[str]) -> None:
    """Replaces this process with Gradle in the build's root."""
    print("> " + " ".join([build.shown_gradle, *arguments]), flush=True)
    os.chdir(build.root)
    os.execv(build.gradle, [build.gradle, *arguments])


def chosen_project(build: Build) -> Path:
    for number, project in enumerate(build.projects, start=1):
        print(f"  {number}) {build.gradle_path(project)}")
    answer = input("Test which project (Enter for 1)? ").strip() or "1"
    choices = {str(number): project for number, project in enumerate(build.projects, start=1)}
    if answer not in choices:
        raise NotABuild(f"no project {answer}")
    return choices[answer]


def main(arguments: list[str]) -> int:
    task = arguments[0] if arguments else ""
    expected_count = {"tests": 3, "zed-tests": 6}.get(task, 2)
    if len(arguments) != expected_count or task not in TASKS:
        print(
            "usage: gradle_task.py " + "|".join(TASKS) + " <file or directory> [<test filter>]",
            file=sys.stderr,
        )
        return 2
    path = arguments[1]
    try:
        build = find_build(Path(path))
        if task == "projects":
            listed = [
                {"path": build.gradle_path(project), "directory": str(project)}
                for project in build.projects
            ]
            print(json.dumps(listed))
            return 0
        if task == "build":
            run(build, ["build", "-x", "test"])
        elif task == "clean":
            run(build, ["clean"])
        elif task in ("tests", "zed-tests"):
            project = build.projects[0]
            if task == "tests":
                test_filter = gradle_test_filter(arguments[2])
            else:
                test_filter = zed_test_filter(*arguments[2:])
            print(f"Testing {test_filter} in {build.gradle_path(project)}")
            run(build, build.filtered_test_arguments(project, test_filter))
        else:
            project = build.projects[0] if task == "test" else chosen_project(build)
            print(f"Testing {build.gradle_path(project)} and its subprojects")
            run(build, build.test_arguments(project))
    except NotABuild as error:
        print(f"gradle_task.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
