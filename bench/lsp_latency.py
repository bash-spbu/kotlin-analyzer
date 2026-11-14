"""Measures kotlin-analyzer on a workspace the way an editor uses it.

    python3 -I bench/lsp_latency.py <root> --project-json <project.json>
        [--server kotlin-analyzer] [--files 40] [--seed 1]

The server talks LSP over stdio. After the workspace loads, a seeded random
sample of Kotlin files is each:
- opened, timing until its diagnostics are published;
- asked for hover and definition at 5 random names, and for completion
  after 2 dots;
- edited by adding a top-level declaration, timing until fresh diagnostics.

Prints the load time, the server's memory afterwards, and p50 / p95 / max
for each kind of request. The README's numbers come from this script.

The project model is the one Gradle exports, so that Gradle's own time isn't
counted. Export it in the project with:

    ./gradlew --no-configuration-cache -Dorg.gradle.isolated-projects=false \\
        --init-script <this repository>/editors/gradle/kotlin-analyzer.init.gradle \\
        kotlinAnalyzerModel

which writes `.kotlin-analyzer/project.json`.
"""

import argparse
import json
import queue
import random
import re
import subprocess
import threading
import time
from pathlib import Path

IDENTIFIER = re.compile(r"\b[a-zA-Z_][A-Za-z0-9_]{2,}\b")
KEYWORDS = {
    "val", "var", "fun", "class", "import", "package", "return", "override",
    "private", "internal", "public", "object", "true", "false", "null", "this",
    "when", "else", "for", "while",
}
NOT_CODE = ("import", "package", "//", "*", "/*")
NAMES_PER_FILE = 5
COMPLETIONS_PER_FILE = 2
EDIT = "\nprivate val latencyProbe = listOf(1, 2).map { it + 1 }\n"
TIMEOUT_SECONDS = 300


class Server:
    """A language server process, with the client's half of the protocol."""

    def __init__(self, command):
        self.process = subprocess.Popen(
            [command], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        self.incoming = queue.Queue()
        self.next_id = 0
        self.diagnostics_versions = {}
        self.loaded = False
        threading.Thread(target=self._read_messages, daemon=True).start()

    def _read_messages(self):
        while True:
            headers = {}
            while True:
                line = self.process.stdout.readline()
                if not line:
                    return
                line = line.decode().strip()
                if not line:
                    break
                key, value = line.split(":", 1)
                headers[key.lower()] = value.strip()
            body = self.process.stdout.read(int(headers["content-length"]))
            self.incoming.put(json.loads(body))

    def _send(self, message):
        data = json.dumps(message).encode()
        self.process.stdin.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
        self.process.stdin.flush()

    def _handle(self, message):
        """Answers the server's requests; remembers diagnostics and the load's end."""
        method = message.get("method")
        if method is not None and "id" in message:
            self._send({"jsonrpc": "2.0", "id": message["id"], "result": None})
        elif method == "textDocument/publishDiagnostics":
            parameters = message["params"]
            self.diagnostics_versions[parameters["uri"]] = parameters.get("version")
        elif method == "$/progress" and message["params"]["value"].get("kind") == "end":
            self.loaded = True

    def request(self, method, parameters):
        self.next_id += 1
        request_id = self.next_id
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": parameters})
        while True:
            message = self.incoming.get(timeout=TIMEOUT_SECONDS)
            is_response = message.get("id") == request_id and "method" not in message
            if is_response:
                return message.get("result")
            self._handle(message)

    def notify(self, method, parameters):
        self._send({"jsonrpc": "2.0", "method": method, "params": parameters})

    def wait_until(self, condition):
        deadline = time.time() + TIMEOUT_SECONDS
        while not condition():
            self._handle(self.incoming.get(timeout=max(0.01, deadline - time.time())))

    def resident_megabytes(self):
        output = subprocess.run(
            ["ps", "-o", "rss=", "-p", str(self.process.pid)], capture_output=True, text=True
        ).stdout
        return int(output.strip() or 0) / 1024

    def shut_down(self):
        self.request("shutdown", None)
        self.notify("exit", None)


def load_workspace(server, root, project_json):
    """Initializes the server on `root` and returns the seconds until it's loaded."""
    started = time.perf_counter()
    server.request("initialize", {
        "processId": None,
        "rootUri": root.as_uri(),
        "capabilities": {"window": {"workDoneProgress": True}},
        "initializationOptions": {"gradleExport": False, "projectJson": str(project_json)},
    })
    server.notify("initialized", {})
    server.wait_until(lambda: server.loaded)
    return time.perf_counter() - started


def kotlin_files(root):
    files = []
    for path in root.rglob("*.kt"):
        is_generated = "/build/" in str(path) or "/.gradle/" in str(path)
        if not is_generated:
            files.append(path)
    return sorted(files)


def name_positions(lines):
    positions = []
    for number, line in enumerate(lines):
        if line.lstrip().startswith(NOT_CODE):
            continue
        for match in IDENTIFIER.finditer(line):
            if match.group() not in KEYWORDS:
                positions.append((number, match.start() + 1))
    return positions


def positions_after_a_dot(lines):
    positions = []
    for number, line in enumerate(lines):
        if "." in line and not line.lstrip().startswith(NOT_CODE):
            positions.append((number, line.index(".") + 1))
    return positions


def timed(action):
    started = time.perf_counter()
    action()
    return time.perf_counter() - started


def measure_file(server, path, timings):
    text = path.read_text()
    uri = path.as_uri()
    document = {"uri": uri}

    def open_file():
        server.notify("textDocument/didOpen", {
            "textDocument": {"uri": uri, "languageId": "kotlin", "version": 1, "text": text},
        })
        server.wait_until(lambda: server.diagnostics_versions.get(uri) == 1)

    timings["open → diagnostics"].append(timed(open_file))

    lines = text.split("\n")
    names = name_positions(lines)
    for line, character in random.sample(names, min(NAMES_PER_FILE, len(names))):
        position = {"textDocument": document, "position": {"line": line, "character": character}}
        timings["hover"].append(timed(lambda: server.request("textDocument/hover", position)))
        timings["definition"].append(timed(lambda: server.request("textDocument/definition", position)))

    dots = positions_after_a_dot(lines)
    for line, character in random.sample(dots, min(COMPLETIONS_PER_FILE, len(dots))):
        position = {"textDocument": document, "position": {"line": line, "character": character}}
        timings["completion"].append(timed(lambda: server.request("textDocument/completion", position)))

    def edit_file():
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": text + EDIT}],
        })
        server.wait_until(lambda: server.diagnostics_versions.get(uri) == 2)

    timings["edit → diagnostics"].append(timed(edit_file))
    server.notify("textDocument/didClose", {"textDocument": document})


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))]


def report(load_seconds, sample_size, megabytes, timings):
    print(f"load: {load_seconds:.2f} s, files sampled: {sample_size}, server memory after: {megabytes:.0f} MB")
    for kind, values in timings.items():
        if not values:
            continue
        p50 = percentile(values, 0.5) * 1000
        p95 = percentile(values, 0.95) * 1000
        slowest = max(values) * 1000
        print(f"{kind:20} n={len(values):3}  p50 {p50:7.1f} ms  p95 {p95:7.1f} ms  max {slowest:7.1f} ms")


def main():
    arguments = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    arguments.add_argument("root", type=Path, help="the project's root directory")
    arguments.add_argument("--project-json", type=Path, required=True, help="the model Gradle exported")
    arguments.add_argument("--server", default="kotlin-analyzer", help="the server's executable")
    arguments.add_argument("--files", type=int, default=40, help="how many files to sample")
    arguments.add_argument("--seed", type=int, default=1, help="the random sample's seed")
    options = arguments.parse_args()

    random.seed(options.seed)
    root = options.root.resolve()
    server = Server(options.server)
    load_seconds = load_workspace(server, root, options.project_json.resolve())

    files = kotlin_files(root)
    sample = random.sample(files, min(options.files, len(files)))
    timings = {kind: [] for kind in
               ["open → diagnostics", "hover", "definition", "completion", "edit → diagnostics"]}
    for path in sample:
        measure_file(server, path, timings)

    report(load_seconds, len(sample), server.resident_megabytes(), timings)
    server.shut_down()


if __name__ == "__main__":
    main()
