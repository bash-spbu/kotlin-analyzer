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
    ["Gradle tasks build, clean and test the file's project or a parent", () => runsGradleTasks(workspace)],
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
