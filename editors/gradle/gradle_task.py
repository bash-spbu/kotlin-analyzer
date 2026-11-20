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
    gradle_task.py profile-tests <path> <filter>
                                    the tests `tests` runs, under
                                    async-profiler: asks what to profile and
                                    opens the flame graphs
    gradle_task.py zed-profile-tests <path> <package> <outer class> <class> <method>
                                    the same, for a Zed runnable's test
    gradle_task.py profile-run <path>
                                    a task of the project holding <path>
                                    (asked for; Enter for `run`), profiled so

An editor that asks itself sets KOTLIN_ANALYZER_PROFILER_EVENT (and
KOTLIN_ANALYZER_PROFILER_OPTIONS, KOTLIN_ANALYZER_PROFILED_TASK): nothing is
asked; and KOTLIN_ANALYZER_PROFILE_RESULT, a file the flame graphs' paths
are written to as JSON instead of being opened.

<path> is a file or a directory. The projects holding it are its ancestor
directories with a build script, up to the build's root: the nearest
directory with a settings script. Gradle is the root's gradlew, else
`gradle` on the PATH. Profiling is described in docs/spec/profiling.md.

Gradle runs with Java: when JAVA_HOME holds no JDK and `java` on the PATH
runs none, JAVA_HOME is set to the newest JDK in ~/.jdks.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

BUILD_SCRIPTS = ("build.gradle.kts", "build.gradle")
SETTINGS_SCRIPTS = ("settings.gradle.kts", "settings.gradle")
TASKS = (
    "build",
    "clean",
    "test",
    "choose",
    "projects",
    "tests",
    "zed-tests",
    "profile-tests",
    "zed-profile-tests",
    "profile-run",
)
# How many arguments each task takes, its name included.
ARGUMENT_COUNTS = {"tests": 3, "zed-tests": 6, "profile-tests": 3, "zed-profile-tests": 6}


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
    os.execve(build.gradle, [build.gradle, *arguments], with_java(dict(os.environ)))


# Java


def runs_java(environment: dict[str, str]) -> bool:
    """Whether JAVA_HOME or `java` on the PATH runs a JDK. macOS's
    /usr/bin/java is a stub that runs one only when one is installed."""
    java_home = environment.get("JAVA_HOME", "")
    if java_home and (Path(java_home) / "bin" / "java").is_file():
        return True
    on_path = shutil.which("java", path=environment.get("PATH", ""))
    if on_path is None:
        return False
    is_macos_stub = sys.platform == "darwin" and Path(on_path) == Path("/usr/bin/java")
    if not is_macos_stub:
        return True
    installed = [Path("/Library/Java/JavaVirtualMachines")]
    if environment.get("HOME"):
        installed.append(Path(environment["HOME"]) / "Library/Java/JavaVirtualMachines")
    return any(
        jdk_home_in(bundle) is not None
        for directory in installed
        if directory.is_dir()
        for bundle in directory.iterdir()
    )


def jdk_home_in(directory: Path) -> Optional[Path]:
    """The JDK home in `directory`: itself, or its macOS bundle's Contents/Home."""
    for home in (directory, directory / "Contents" / "Home"):
        if (home / "bin" / "java").is_file():
            return home
    return None


def feature_release(home: Path) -> int:
    """A JDK's feature release from its `release` file (0 when unknown)."""
    try:
        text = (home / "release").read_text()
    except OSError:
        return 0
    for line in text.splitlines():
        if line.startswith("JAVA_VERSION="):
            version = line.split("=", 1)[1].strip().strip('"')
            parts = version.replace("_", ".").replace("+", ".").replace("-", ".").split(".")
            first = int(parts[0]) if parts[0].isdigit() else 0
            if first == 1 and len(parts) > 1 and parts[1].isdigit():
                return int(parts[1])
            return first
    return 0


def with_java(environment: dict[str, str]) -> dict[str, str]:
    """`environment`, with JAVA_HOME set to the newest JDK in ~/.jdks when
    it runs no Java otherwise."""
    if runs_java(environment) or not environment.get("HOME"):
        return environment
    jdks = Path(environment["HOME"]) / ".jdks"
    homes = [jdk_home_in(entry) for entry in jdks.iterdir()] if jdks.is_dir() else []
    found = [home for home in homes if home is not None]
    if not found:
        return environment
    newest = max(found, key=lambda home: (feature_release(home), str(home)))
    return {**environment, "JAVA_HOME": str(newest)}


# Profiling (docs/spec/profiling.md)

# What can be profiled: the agent's event options, and the flame graphs
# `jfrconv` makes of a recording, by its option.
PROFILER_EVENTS = {
    "cpu": ("event=cpu", ("cpu",), "CPU time"),
    "alloc": ("event=alloc", ("alloc",), "allocated memory"),
    "lock": ("event=lock", ("lock",), "lock contention"),
    "wall": ("event=wall", ("wall",), "wall-clock time, waiting included"),
    "all": ("event=cpu,alloc,lock", ("cpu", "alloc", "lock"), "cpu, alloc and lock at once"),
}
PROFILER_RELEASE = "https://api.github.com/repos/async-profiler/async-profiler/releases/latest"


class ProfilingFailed(Exception):
    """Profiling can't go on: no async-profiler, a refused option, …"""


@dataclass(frozen=True)
class ProfileSettings:
    event: str
    # More agent options, comma-separated; "" for none.
    options: str

    def agent_options(self) -> list[str]:
        events = PROFILER_EVENTS[self.event][0]
        extra = [option.strip() for option in self.options.split(",") if option.strip()]
        return [events, *extra]

    def flame_graphs(self) -> tuple[str, ...]:
        return PROFILER_EVENTS[self.event][1]


DEFAULT_SETTINGS = ProfileSettings("cpu", "")


def checked_options(written: str) -> str:
    """The agent options as written, `-` for none; the output file is ours."""
    options = written.strip()
    if options == "-":
        return ""
    names = [option.split("=", 1)[0].strip() for option in options.split(",")]
    if "file" in names:
        raise ProfilingFailed("the recording's file is the task's: leave out file=")
    return options


def cache_directory(environment: dict[str, str]) -> Path:
    home = environment.get("HOME") or str(Path.home())
    base = environment.get("XDG_CACHE_HOME") or str(Path(home) / ".cache")
    return Path(base) / "kotlin-analyzer"


def remembered_settings(cache: Path) -> ProfileSettings:
    try:
        saved = json.loads((cache / "profiler.json").read_text())
        event = saved["event"] if saved.get("event") in PROFILER_EVENTS else "cpu"
        return ProfileSettings(event, str(saved.get("options", "")))
    except (OSError, ValueError, KeyError, TypeError):
        return DEFAULT_SETTINGS


def remember_settings(cache: Path, settings: ProfileSettings) -> None:
    try:
        cache.mkdir(parents=True, exist_ok=True)
        saved = {"event": settings.event, "options": settings.options}
        (cache / "profiler.json").write_text(json.dumps(saved) + "\n")
    except OSError:
        pass


def asked_settings(last: ProfileSettings, ask: Callable[[str], str]) -> ProfileSettings:
    """What to profile and with which options, asked; Enter keeps `last`."""
    names = list(PROFILER_EVENTS)
    for number, name in enumerate(names, start=1):
        print(f"  {number}) {name:5}  {PROFILER_EVENTS[name][2]}")
    answer = ask(f"Profile what (Enter for {last.event})? ").strip() or last.event
    by_number = {str(number): name for number, name in enumerate(names, start=1)}
    event = by_number.get(answer, answer)
    if event not in PROFILER_EVENTS:
        raise ProfilingFailed(f"no such event: {answer}")
    shown = last.options or "none"
    print("More async-profiler options, comma-separated (interval=1ms, alloc=512k, threads, …):")
    written = ask(f"Options (Enter for {shown}, - for none)? ")
    options = last.options if written.strip() == "" else checked_options(written)
    return ProfileSettings(event, options)


@dataclass(frozen=True)
class Profiler:
    home: Path

    @property
    def library(self) -> Path:
        for name in ("libasyncProfiler.dylib", "libasyncProfiler.so"):
            if (self.home / "lib" / name).is_file():
                return self.home / "lib" / name
        raise ProfilingFailed(f"{self.home} has no libasyncProfiler")

    @property
    def converter(self) -> Path:
        return self.home / "bin" / "jfrconv"


def profiler_in(home: Path) -> Optional[Profiler]:
    has_library = any(
        (home / "lib" / name).is_file() for name in ("libasyncProfiler.dylib", "libasyncProfiler.so")
    )
    return Profiler(home) if has_library else None


def find_profiler(environment: dict[str, str], cache: Path) -> Optional[Profiler]:
    """ASYNC_PROFILER_HOME, `asprof` on the PATH, else the newest downloaded."""
    if environment.get("ASYNC_PROFILER_HOME"):
        found = profiler_in(Path(environment["ASYNC_PROFILER_HOME"]))
        if found is not None:
            return found
    asprof = shutil.which("asprof", path=environment.get("PATH", ""))
    if asprof is not None:
        found = profiler_in(Path(asprof).resolve().parent.parent)
        if found is not None:
            return found
    downloads = cache / "async-profiler"
    if not downloads.is_dir():
        return None
    releases = sorted(
        (entry for entry in downloads.iterdir() if entry.is_dir() and not entry.name.startswith(".")),
        key=lambda entry: release_order(entry.name),
        reverse=True,
    )
    for release in releases:
        found = profiler_in(release)
        if found is not None:
            return found
    return None


def release_order(tag: str) -> tuple[int, ...]:
    """`v4.6` as (4, 6), for the newest first."""
    numbers = tag.lstrip("v").split(".")
    return tuple(int(number) if number.isdigit() else 0 for number in numbers)


def release_asset(release: dict, system: str, machine: str) -> tuple[str, str, str]:
    """The release's archive for this platform: its name, address and SHA-256."""
    if system == "Darwin":
        suffix = "-macos.zip"
    elif system == "Linux" and machine in ("x86_64", "amd64"):
        suffix = "-linux-x64.tar.gz"
    elif system == "Linux" and machine in ("aarch64", "arm64"):
        suffix = "-linux-arm64.tar.gz"
    else:
        raise ProfilingFailed(f"async-profiler has no release for {system} {machine}")
    for asset in release.get("assets", []):
        name = asset.get("name", "")
        if not name.endswith(suffix):
            continue
        url = asset.get("browser_download_url", "")
        digest = asset.get("digest") or ""
        if not url.startswith("https://"):
            raise ProfilingFailed(f"{name} has no HTTPS address")
        if not digest.startswith("sha256:"):
            raise ProfilingFailed(f"{name} has no SHA-256 to check it against")
        return name, url, digest[len("sha256:"):].lower()
    raise ProfilingFailed(f"the release has no *{suffix}")


def curl(arguments: list[str]) -> None:
    command = [
        "curl", "--fail", "--silent", "--show-error", "--location",
        "--proto", "=https", "--proto-redir", "=https", *arguments,
    ]
    try:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    except OSError as error:
        raise ProfilingFailed(f"cannot run curl: {error}") from error
    if result.returncode != 0:
        raise ProfilingFailed(result.stderr.strip() or f"curl failed ({result.returncode})")


def install_profiler(cache: Path) -> Profiler:
    """Downloads the latest async-profiler release into the cache."""
    downloads = cache / "async-profiler"
    downloads.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=downloads, prefix=".download-") as staging_name:
        staging = Path(staging_name)
        print("Downloading async-profiler's latest release…", flush=True)
        curl(["--header", "Accept: application/vnd.github+json", "--output", str(staging / "release.json"), PROFILER_RELEASE])
        release = json.loads((staging / "release.json").read_text())
        tag = str(release.get("tag_name", ""))
        if not tag or "/" in tag or tag.startswith("."):
            raise ProfilingFailed("the release has no usable tag")
        name, url, sha256 = release_asset(release, platform.system(), platform.machine())
        archive = staging / name
        curl(["--output", str(archive), url])
        if file_sha256(archive) != sha256:
            raise ProfilingFailed(f"{name}'s SHA-256 isn't the release's: not used")
        unpacked = staging / "unpacked"
        unpack(archive, unpacked)
        entries = [entry for entry in unpacked.iterdir() if not entry.name.startswith("._")]
        top = entries[0] if len(entries) == 1 and entries[0].is_dir() else unpacked
        if profiler_in(top) is None:
            raise ProfilingFailed(f"{name} holds no libasyncProfiler")
        target = downloads / tag
        if target.exists():
            shutil.rmtree(target)
        top.rename(target)
    print(f"async-profiler {tag} is in {target}", flush=True)
    return Profiler(target)


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1 << 16), b""):
            digest.update(block)
    return digest.hexdigest()


def unpack(archive: Path, destination: Path) -> None:
    """Unpacks a .zip or .tar.gz into `destination`; an entry that would land
    outside it, or a link, is refused."""
    destination.mkdir(parents=True, exist_ok=True)
    root = destination.resolve()

    def inside(name: str) -> Path:
        target = (root / name).resolve()
        if target != root and root not in target.parents:
            raise ProfilingFailed(f"{archive.name} has an entry outside it: {name}")
        return target

    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as opened:
            for entry in opened.infolist():
                target = inside(entry.filename)
                if entry.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                with opened.open(entry) as source, target.open("wb") as written:
                    shutil.copyfileobj(source, written)
                mode = (entry.external_attr >> 16) & 0o777
                target.chmod(mode or 0o644)
        return
    with tarfile.open(archive, "r:gz") as opened:
        for member in opened.getmembers():
            target = inside(member.name)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                source = opened.extractfile(member)
                assert source is not None
                with source, target.open("wb") as written:
                    shutil.copyfileobj(source, written)
                target.chmod(member.mode & 0o777)
            else:
                raise ProfilingFailed(f"{archive.name} has a link or a device: {member.name}")


def agent_argument(profiler: Profiler, settings: ProfileSettings, output: Path) -> str:
    options = ["start", *settings.agent_options(), f"file={output / 'profile-%p.jfr'}"]
    return f"-agentpath:{profiler.library}={','.join(options)}"


def profiling_init_script(agent: str) -> str:
    """Every Test and JavaExec JVM gets the agent, through
    jvmArgumentProviders, which a build script's `jvmArgs = [...]` doesn't
    replace."""
    quoted = agent.replace("\\", "\\\\").replace("'", "\\'")
    return (
        f"def kotlinAnalyzerProfiler = {{ ['{quoted}'] }} as org.gradle.process.CommandLineArgumentProvider\n"
        "allprojects {\n"
        "    tasks.withType(Test).configureEach {\n"
        "        jvmArgumentProviders.add(kotlinAnalyzerProfiler)\n"
        "    }\n"
        "    tasks.withType(JavaExec).configureEach {\n"
        "        jvmArgumentProviders.add(kotlinAnalyzerProfiler)\n"
        "    }\n"
        "}\n"
    )


def flame_graph_commands(
    profiler: Profiler, recordings: list[Path], output: Path, settings: ProfileSettings, title: str
) -> list[tuple[list[str], Path]]:
    """jfrconv's command for each flame graph, and the page it writes."""
    commands = []
    for kind in settings.flame_graphs():
        page = output / f"{kind}.html"
        command = [
            str(profiler.converter), f"--{kind}", "--title", f"{title}: {kind}",
            *[str(recording) for recording in recordings], str(page),
        ]
        commands.append((command, page))
    return commands


def open_command(environment: dict[str, str], page: Path) -> Optional[list[str]]:
    """How a page opens: KOTLIN_ANALYZER_OPEN_WITH, else the default browser."""
    chosen = environment.get("KOTLIN_ANALYZER_OPEN_WITH")
    if chosen:
        return [chosen, str(page)]
    if sys.platform == "darwin":
        return ["open", str(page)]
    if shutil.which("xdg-open", path=environment.get("PATH", "")):
        return ["xdg-open", str(page)]
    return None


def chosen_settings(environment: dict[str, str], last: ProfileSettings, ask: Callable[[str], str]) -> ProfileSettings:
    """What to profile: as KOTLIN_ANALYZER_PROFILER_EVENT (and _OPTIONS) say
    when an editor asked; else asked in the terminal; else the last."""
    event = environment.get("KOTLIN_ANALYZER_PROFILER_EVENT")
    if event:
        if event not in PROFILER_EVENTS:
            raise ProfilingFailed(f"no such event: {event}")
        return ProfileSettings(event, checked_options(environment.get("KOTLIN_ANALYZER_PROFILER_OPTIONS", "")))
    if sys.stdin.isatty():
        return asked_settings(last, ask)
    return last


def profile(build: Build, arguments: list[str], title: str, ask: Callable[[str], str]) -> int:
    """Runs Gradle with `arguments` under async-profiler, then makes the
    flame graphs and opens them (or, for an editor, writes where they are)."""
    environment = with_java(dict(os.environ))
    cache = cache_directory(environment)
    settings = chosen_settings(environment, remembered_settings(cache), ask)
    remember_settings(cache, settings)
    profiler = find_profiler(environment, cache) or install_profiler(cache)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    output = build.root / "build" / "kotlin-analyzer" / "profiles" / f"{stamp}-{settings.event}"
    output.mkdir(parents=True, exist_ok=True)
    agent = agent_argument(profiler, settings, output)
    with tempfile.TemporaryDirectory(prefix="kotlin-analyzer-profile-") as scratch:
        script = Path(scratch) / "profile.init.gradle"
        script.write_text(profiling_init_script(agent))
        command = [*arguments, "--console=plain", "-I", str(script)]
        print(f"Profiling {title}: {', '.join(settings.agent_options())}", flush=True)
        print("> " + " ".join([build.shown_gradle, *arguments]), flush=True)
        # Ctrl-C stops Gradle (and the JVM it runs, which writes its
        # recording); this script goes on to the flame graphs.
        previous = signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            gradle = subprocess.run(
                [build.gradle, *command],
                cwd=build.root,
                env=environment,
                preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL),
            )
        finally:
            signal.signal(signal.SIGINT, previous)
    recordings = sorted(output.glob("profile-*.jfr"))
    pages = []
    for command, page in flame_graph_commands(profiler, recordings, output, settings, title):
        if not recordings:
            break
        converted = subprocess.run(command, env=environment)
        if converted.returncode == 0 and page.is_file():
            pages.append(page)
    result_file = environment.get("KOTLIN_ANALYZER_PROFILE_RESULT")
    if result_file:
        result = {
            "title": title,
            "event": settings.event,
            "pages": [str(page) for page in pages],
            "recordings": str(output),
            "gradleExitCode": gradle.returncode,
        }
        Path(result_file).write_text(json.dumps(result) + "\n")
    if not recordings:
        print(f"No JVM was profiled: no recording in {output}", file=sys.stderr)
        return gradle.returncode or 1
    for page in pages:
        print(f"Flame graph: {page}", flush=True)
        opener = None if result_file else open_command(environment, page)
        if opener is not None:
            subprocess.run(opener, env=environment)
    print(f"Recordings: {output}")
    return gradle.returncode


def chosen_project(build: Build) -> Path:
    for number, project in enumerate(build.projects, start=1):
        print(f"  {number}) {build.gradle_path(project)}")
    answer = input("Test which project (Enter for 1)? ").strip() or "1"
    choices = {str(number): project for number, project in enumerate(build.projects, start=1)}
    if answer not in choices:
        raise NotABuild(f"no project {answer}")
    return choices[answer]


def profiled_task(build: Build, project: Path, ask: Callable[[str], str]) -> list[str]:
    """The task of `project` to profile, asked for; Enter for `run`."""
    answer = ask(f"Profile which task of {build.gradle_path(project)} (Enter for run)? ").strip()
    name = answer or "run"
    if name.startswith(":") or not name.replace("-", "").replace("_", "").isalnum():
        raise ProfilingFailed(f"not a task name: {name}")
    prefix = build.gradle_path(project)
    return [f"{prefix}:{name}" if prefix != ":" else f":{name}"]


def main(arguments: list[str]) -> int:
    task = arguments[0] if arguments else ""
    expected_count = ARGUMENT_COUNTS.get(task, 2)
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
        if task in ("profile-tests", "zed-profile-tests"):
            project = build.projects[0]
            if task == "profile-tests":
                test_filter = gradle_test_filter(arguments[2])
            else:
                test_filter = zed_test_filter(*arguments[2:])
            return profile(
                build, build.filtered_test_arguments(project, test_filter), test_filter, input
            )
        if task == "profile-run":
            project = build.projects[0]
            chosen_task = os.environ.get("KOTLIN_ANALYZER_PROFILED_TASK")
            if chosen_task:
                ask = lambda _prompt: chosen_task  # noqa: E731
            elif sys.stdin.isatty():
                ask = input
            else:
                ask = lambda _prompt: ""  # noqa: E731
            gradle_task = profiled_task(build, project, ask)
            return profile(build, gradle_task, gradle_task[0], input)
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
    except (NotABuild, ProfilingFailed) as error:
        print(f"gradle_task.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
