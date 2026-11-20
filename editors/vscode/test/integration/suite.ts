// Runs inside VS Code's extension host (see run.ts): each check gets the
// workspace run.ts wrote and fails the run by throwing.

import { strict as assert } from "node:assert";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

import type { Api } from "../../src/extension";

const PATIENCE_MILLISECONDS = 60_000;

export async function run(): Promise<void> {
  const workspace = process.env.KOTLIN_ANALYZER_TEST_WORKSPACE;
  assert.ok(workspace, "run.ts sets the workspace");
  const sources = path.join(workspace, "src/main/kotlin/app");
  const checks: Array<[string, () => Promise<void>]> = [
    ["the extension downloads its release and starts it", () => startsTheDownloadedRelease(sources)],
    ["a Kotlin file gets the grammar and the server's diagnostics", () => reportsSyntaxErrors(sources)],
    ["hover and definition reach the server", () => hoversAndNavigates(sources)],
    ["the server's reload command is in the palette", () => reloads()],
    ["Format Document runs the configured formatter", () => formats(workspace, sources)],
    ["Gradle tasks build, clean and test the file's project or a parent", () => runsGradleTasks(workspace)],
    ["code lenses run a test and count references", () => runsTestsFromLenses(workspace)],
    ["a Debug test lens debugs the test through Gradle", () => debugsTestsFromLenses(workspace)],
    ["a Profile test lens profiles the test and opens its flame graph", () => profilesTestsFromLenses(workspace)],
    ["the debugger stops at a breakpoint and runs a logpoint", () => debugsAProgram(workspace)],
  ];
  const failures: string[] = [];
  for (const [name, check] of checks) {
    try {
      await check();
      console.log(`  ✔ ${name}`);
    } catch (error) {
      console.log(`  ✘ ${name}: ${error instanceof Error ? error.stack : String(error)}`);
      failures.push(name);
    }
  }
  if (failures.length > 0) {
    throw new Error(`${failures.length} check(s) failed: ${failures.join("; ")}`);
  }
}

async function startsTheDownloadedRelease(sources: string): Promise<void> {
  await vscode.window.showTextDocument(vscode.Uri.file(path.join(sources, "Use.kt")));
  const extension = vscode.extensions.getExtension<Api>("bash-spbu.kotlin-analyzer");
  assert.ok(extension, "the extension is installed");
  const api = await extension.activate();
  const binary = api.binary();
  assert.equal(binary?.source, "release");
  assert.ok(api.client()?.isRunning(), "the client runs");
}

async function reportsSyntaxErrors(sources: string): Promise<void> {
  const broken = vscode.Uri.file(path.join(sources, "Broken.kt"));
  const document = await vscode.workspace.openTextDocument(broken);
  await vscode.window.showTextDocument(document);
  assert.equal(document.languageId, "kotlin");
  const diagnostics = await waitFor(() => {
    const published = vscode.languages.getDiagnostics(broken);
    return published.length > 0 ? published : undefined;
  }, "diagnostics for Broken.kt");
  assert.ok(diagnostics.some((diagnostic) => diagnostic.severity === vscode.DiagnosticSeverity.Error));
}

async function hoversAndNavigates(sources: string): Promise<void> {
  const use = vscode.Uri.file(path.join(sources, "Use.kt"));
  const document = await vscode.workspace.openTextDocument(use);
  await vscode.window.showTextDocument(document);
  const onBox = document.positionAt(document.getText().indexOf("Box(") + 1);
  const hoverText = await waitFor(async () => {
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>("vscode.executeHoverProvider", use, onBox);
    const text = hovers
      .flatMap((hover) => hover.contents)
      .map((content) => (typeof content === "string" ? content : content.value))
      .join("\n");
    return text.includes("class Box") ? text : undefined;
  }, "a hover on Box");
  assert.match(hoverText, /class Box/);
  const definitions = await vscode.commands.executeCommand<Array<vscode.Location | vscode.LocationLink>>(
    "vscode.executeDefinitionProvider",
    use,
    onBox,
  );
  const targets = definitions.map((definition) => ("targetUri" in definition ? definition.targetUri : definition.uri));
  assert.deepEqual(
    targets.map((target) => path.basename(target.fsPath)),
    ["Box.kt"],
  );
}

async function formats(workspace: string, sources: string): Promise<void> {
  const file = vscode.Uri.file(path.join(sources, "Format.kt"));
  const document = await vscode.workspace.openTextDocument(file);
  await vscode.window.showTextDocument(document);
  const edits = await waitFor(async () => {
    const answer = await vscode.commands.executeCommand<vscode.TextEdit[] | undefined>(
      "vscode.executeFormatDocumentProvider",
      file,
      { tabSize: 4, insertSpaces: true },
    );
    return answer !== undefined && answer.length > 0 ? answer : undefined;
  }, "formatting edits for Format.kt");
  const edit = new vscode.WorkspaceEdit();
  edit.set(file, edits);
  await vscode.workspace.applyEdit(edit);
  assert.equal(document.getText(), "package app\n\nval one = 1\n");
  const call = await fs.readFile(path.join(workspace, "fake-ktrs.call"), "utf8");
  assert.equal(call, "fmt --style kotlinlang -\n");
}

async function runsTestsFromLenses(workspace: string): Promise<void> {
  const file = vscode.Uri.file(path.join(workspace, "lib/src/test/kotlin/lib/LibTest.kt"));
  await vscode.window.showTextDocument(file);
  const lenses = await waitFor(async () => {
    const found = await vscode.commands.executeCommand<vscode.CodeLens[]>("vscode.executeCodeLensProvider", file, 100);
    return found.some((lens) => lens.command?.title === "▶ Run test") ? found : undefined;
  }, "a Run test lens in LibTest.kt");
  const counts = lenses.map((lens) => lens.command?.title).filter((title) => title?.endsWith("references"));
  assert.ok(counts.includes("0 references"), `reference counts resolved: ${counts.join(", ")}`);
  const showReferences = lenses.find((lens) => lens.command?.title.endsWith("references"))!.command!;
  await vscode.commands.executeCommand(showReferences.command, ...(showReferences.arguments ?? []));
  const runTest = lenses.find((lens) => lens.command?.title === "▶ Run test")!.command!;
  const callFile = process.env.GRADLE_CALL;
  assert.ok(callFile, "run.ts sets GRADLE_CALL");
  await fs.rm(callFile, { force: true });
  const ended = new Promise<number | undefined>((resolve) => {
    const listener = vscode.tasks.onDidEndTaskProcess((event) => {
      listener.dispose();
      resolve(event.exitCode);
    });
  });
  await vscode.commands.executeCommand(runTest.command, ...(runTest.arguments ?? []));
  assert.equal(await ended, 0);
  const call: string[] = (await fs.readFile(callFile, "utf8")).trim().split("\n");
  assert.deepEqual(call.slice(1), ["-p", "lib", "test", "--tests", "lib.LibTest.works", "--rerun"]);
}

async function debugsTestsFromLenses(workspace: string): Promise<void> {
  const file = vscode.Uri.file(path.join(workspace, "lib/src/test/kotlin/lib/LibTest.kt"));
  await vscode.window.showTextDocument(file);
  const lenses = await waitFor(async () => {
    const found = await vscode.commands.executeCommand<vscode.CodeLens[]>("vscode.executeCodeLensProvider", file, 100);
    return found.some((lens) => lens.command?.title === "Debug test") ? found : undefined;
  }, "a Debug test lens in LibTest.kt");
  const titles = lenses.map((lens) => lens.command?.title).filter((title) => title?.includes("test"));
  assert.deepEqual(titles, [
    "▶ Run tests",
    "Debug tests",
    "Profile tests",
    "▶ Run test",
    "Debug test",
    "Profile test",
  ]);
  const debugTest = lenses.find((lens) => lens.command?.title === "Debug test")!.command!;
  const callFile = process.env.GRADLE_CALL;
  assert.ok(callFile, "run.ts sets GRADLE_CALL");
  await fs.rm(callFile, { force: true });
  // The recording gradlew starts no JVM: the launch fails, saying so (in a
  // dialog, which VS Code refuses in tests and throws instead).
  await assert.rejects(
    Promise.resolve(vscode.commands.executeCommand(debugTest.command, ...(debugTest.arguments ?? []))),
    /before the JVM connected/,
  );
  const call: string[] = (await fs.readFile(callFile, "utf8")).trim().split("\n");
  const tests = call.indexOf("--tests");
  assert.deepEqual(call.slice(tests - 3, tests + 3), ["-p", "lib", "test", "--tests", "lib.LibTest.works", "--rerun"]);
  assert.ok(call.includes("-I"), `the init script giving the test JVM the debugger: ${call.join(" ")}`);
}

async function profilesTestsFromLenses(workspace: string): Promise<void> {
  const file = vscode.Uri.file(path.join(workspace, "lib/src/test/kotlin/lib/LibTest.kt"));
  await vscode.window.showTextDocument(file);
  const lenses = await waitFor(async () => {
    const found = await vscode.commands.executeCommand<vscode.CodeLens[]>("vscode.executeCodeLensProvider", file, 100);
    return found.some((lens) => lens.command?.title === "Profile test") ? found : undefined;
  }, "a Profile test lens in LibTest.kt");
  const profileTest = lenses.find((lens) => lens.command?.title === "Profile test")!.command!;
  const callFile = process.env.GRADLE_CALL;
  assert.ok(callFile, "run.ts sets GRADLE_CALL");
  await fs.rm(callFile, { force: true });
  // What the pickers would have asked, given as the command's argument.
  await vscode.commands.executeCommand(profileTest.command, ...(profileTest.arguments ?? []), {
    event: "cpu",
    options: "interval=1ms",
  });
  const call: string[] = (await fs.readFile(callFile, "utf8")).trim().split("\n");
  const tests = call.indexOf("--tests");
  assert.deepEqual(call.slice(tests - 3, tests + 3), ["-p", "lib", "test", "--tests", "lib.LibTest.works", "--rerun"]);
  const flameGraph = await waitFor(
    () =>
      vscode.window.tabGroups.all
        .flatMap((group) => group.tabs)
        .find((tab) => tab.input instanceof vscode.TabInputWebview && tab.label === "CPU: lib.LibTest.works"),
    "the flame graph's tab",
  );
  assert.ok(flameGraph);
  const profiles = path.join(workspace, "build/kotlin-analyzer/profiles");
  const [run] = await fs.readdir(profiles);
  assert.ok(run.endsWith("-cpu"), run);
  const page = await fs.readFile(path.join(profiles, run, "cpu.html"), "utf8");
  assert.match(page, /--cpu/);
}

async function debugsAProgram(workspace: string): Promise<void> {
  const source = path.join(workspace, "debug/src/demo/Main.java");
  const text = await fs.readFile(source, "utf8");
  const lineOf = (needle: string): number => text.split("\n").findIndex((line) => line.includes(needle));
  const at = (needle: string) => new vscode.Location(vscode.Uri.file(source), new vscode.Position(lineOf(needle), 0));
  const breakpoints = [
    new vscode.SourceBreakpoint(at("\"hello \""), true, undefined, undefined, "name is {name}"),
    new vscode.SourceBreakpoint(at("int doubled"), true, "count == 3"),
  ];
  vscode.debug.addBreakpoints(breakpoints);
  const events: Array<{ event: string; body: any }> = [];
  const tracker = vscode.debug.registerDebugAdapterTrackerFactory("kotlin-analyzer", {
    createDebugAdapterTracker: () => ({
      onDidSendMessage: (message: { type: string; event: string; body: any }) => {
        if (message.type === "event") {
          events.push(message);
        }
      },
    }),
  });
  try {
    const started = await vscode.debug.startDebugging(vscode.workspace.workspaceFolders?.[0], {
      type: "kotlin-analyzer",
      request: "launch",
      name: "Main",
      program: "java",
      args: ["-cp", path.join(workspace, "debug/classes"), "demo.Main"],
      cwd: path.join(workspace, "debug"),
    });
    assert.ok(started, "the session starts");
    const stopped = await waitFor(() => events.find((event) => event.event === "stopped"), "a stop");
    const session = vscode.debug.activeDebugSession;
    assert.ok(session, "a session");
    const stack = await session.customRequest("stackTrace", { threadId: stopped.body.threadId });
    const top = stack.stackFrames[0];
    assert.equal(top.line, lineOf("int doubled") + 1);
    assert.equal(top.source.path, source);
    const scopes = await session.customRequest("scopes", { frameId: top.id });
    const variables = await session.customRequest("variables", {
      variablesReference: scopes.scopes[0].variablesReference,
    });
    const shown = variables.variables.map((variable: { name: string; value: string }) => `${variable.name} = ${variable.value}`);
    assert.ok(shown.includes("count = 3"), shown.join(", "));
    const terminated = new Promise<void>((resolve) => {
      const listener = vscode.debug.onDidTerminateDebugSession(() => {
        listener.dispose();
        resolve();
      });
    });
    await session.customRequest("continue", { threadId: stopped.body.threadId });
    await terminated;
    const output = events
      .filter((event) => event.event === "output")
      .map((event) => event.body.output)
      .join("");
    assert.ok(output.includes("name is kotlin\n"), output);
    assert.ok(output.includes("doubled 6\n"), output);
    assert.deepEqual(
      events.filter((event) => event.event === "exited").map((event) => event.body.exitCode),
      [0],
    );
  } finally {
    tracker.dispose();
    vscode.debug.removeBreakpoints(breakpoints);
  }
}

async function reloads(): Promise<void> {
  const commands = await vscode.commands.getCommands(true);
  assert.ok(commands.includes("kotlin-analyzer.reloadWorkspace"), "registered from the server's capabilities");
  await vscode.commands.executeCommand("kotlin-analyzer.reloadWorkspace");
}

async function runsGradleTasks(workspace: string): Promise<void> {
  await vscode.window.showTextDocument(vscode.Uri.file(path.join(workspace, "lib/src/main/kotlin/lib/Lib.kt")));
  const tasks = await vscode.tasks.fetchTasks({ type: "kotlin-gradle" });
  assert.deepEqual(
    tasks.map((task) => task.name),
    ["gradle build -x test", "gradle clean", "gradle test --rerun-tasks: :lib", "gradle test --rerun-tasks: :"],
  );
  const expected: Record<string, string[]> = {
    "gradle build -x test": ["build", "-x", "test"],
    "gradle clean": ["clean"],
    "gradle test --rerun-tasks: :lib": ["-p", "lib", "test", "--rerun-tasks"],
    "gradle test --rerun-tasks: :": ["test", "--rerun-tasks"],
  };
  const callFile = process.env.GRADLE_CALL;
  assert.ok(callFile, "run.ts sets GRADLE_CALL");
  for (const task of tasks) {
    await fs.rm(callFile, { force: true });
    const ended = new Promise<number | undefined>((resolve) => {
      const listener = vscode.tasks.onDidEndTaskProcess((event) => {
        if (event.execution.task.name === task.name) {
          listener.dispose();
          resolve(event.exitCode);
        }
      });
    });
    await vscode.tasks.executeTask(task);
    assert.equal(await ended, 0, task.name);
    const call: string[] = (await fs.readFile(callFile, "utf8")).trim().split("\n");
    const [directory, ...gradleArguments] = call;
    assert.equal(await fs.realpath(directory ?? ""), await fs.realpath(workspace), task.name);
    assert.deepEqual(gradleArguments, expected[task.name], task.name);
  }
}

/** Polls `probe` until it gives a value. */
async function waitFor<T>(probe: () => T | undefined | Promise<T | undefined>, what: string): Promise<T> {
  const deadline = Date.now() + PATIENCE_MILLISECONDS;
  while (Date.now() < deadline) {
    const value = await probe();
    if (value !== undefined) {
      return value;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`timed out waiting for ${what}`);
}
