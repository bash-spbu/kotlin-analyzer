// Profiling with async-profiler (docs/spec/profiling.md): a test from its
// "Profile test" lens, or a Gradle task of the active file's project (run,
// bootRun, …) from the command palette. VS Code asks what to profile and
// with which options; gradle_task.py runs Gradle with the agent, makes the
// flame graphs and writes where they are; they open here, each in a tab.

import { promises as fs } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";

import type { TestRun } from "./debugging";
import {
  DEFAULT_SETTINGS,
  PROFILE_TASK_COMMAND,
  PROFILE_TASK_TYPE,
  PROFILE_TESTS_COMMAND,
  type ProfileResult,
  type ProfileSettings,
  eventsLastFirst,
  flameGraphTitle,
  parseProfileResult,
  profileEnvironment,
  refusedOptions,
} from "./profiling";

/** Where the last answers are kept, per workspace. */
const LAST_SETTINGS = "kotlin-analyzer.profiler.settings";
const LAST_TASK = "kotlin-analyzer.profiler.task";

/** A profiled run as `tasks.json` holds one (its flame graphs open in the browser). */
interface ProfileTaskDefinition extends vscode.TaskDefinition {
  path: string;
  event: ProfileSettings["event"];
  options?: string;
  /** A Gradle `--tests` filter: those tests, in the project holding `path`. */
  tests?: string;
  /** Else this task of that project (`run` when neither is given). */
  task?: string;
}

export function registerProfiling(context: vscode.ExtensionContext): vscode.Disposable {
  const script = context.asAbsolutePath(path.join("dist", "gradle_task.py"));
  return vscode.Disposable.from(
    vscode.commands.registerCommand(PROFILE_TESTS_COMMAND, (run: TestRun, chosen?: ProfileSettings) =>
      profileTests(context, script, run, chosen),
    ),
    vscode.commands.registerCommand(PROFILE_TASK_COMMAND, () => profileTask(context, script)),
    vscode.tasks.registerTaskProvider(PROFILE_TASK_TYPE, {
      provideTasks: () => [],
      resolveTask: (task) => configuredTask(script, task),
    }),
  );
}

/** A `tasks.json` profiled run: gradle_task.py with its choices, opening the pages itself. */
function configuredTask(script: string, task: vscode.Task): vscode.Task {
  const definition = task.definition as ProfileTaskDefinition;
  const settings: ProfileSettings = { event: definition.event, options: definition.options ?? "" };
  const scriptArguments =
    definition.tests === undefined
      ? ["profile-run", definition.path]
      : ["profile-tests", definition.path, definition.tests];
  const environment: Record<string, string> = {
    KOTLIN_ANALYZER_PROFILER_EVENT: settings.event,
    KOTLIN_ANALYZER_PROFILER_OPTIONS: settings.options,
    ...(definition.tests === undefined ? { KOTLIN_ANALYZER_PROFILED_TASK: definition.task ?? "run" } : {}),
  };
  const execution = new vscode.ProcessExecution("python3", ["-I", script, ...scriptArguments], { env: environment });
  return new vscode.Task(definition, task.scope ?? vscode.TaskScope.Workspace, task.name, "kotlin-analyzer", execution, []);
}

/** Profiles the tests `run` names: as `chosen` says (a keybinding's argument), else as asked. */
async function profileTests(
  context: vscode.ExtensionContext,
  script: string,
  run: TestRun,
  chosen: ProfileSettings | undefined,
): Promise<void> {
  const settings = chosen ?? (await askSettings(context, `Profile ${run.filter}`));
  if (settings === undefined) {
    return;
  }
  const file = vscode.Uri.parse(run.uri);
  await profile(context, script, file, ["profile-tests", file.fsPath, run.filter], settings, run.filter);
}

async function profileTask(context: vscode.ExtensionContext, script: string): Promise<void> {
  const file = vscode.window.activeTextEditor?.document.uri;
  if (file?.scheme !== "file") {
    void vscode.window.showWarningMessage("kotlin-analyzer: open a file of the project to profile first.");
    return;
  }
  const last = context.workspaceState.get<string>(LAST_TASK) ?? "run";
  const gradleTask = await vscode.window.showInputBox({
    title: "Profile a Gradle task",
    prompt: "A task of this file's project that starts a JVM (JavaExec): run, bootRun, …",
    value: last,
    validateInput: (value) => (/^[A-Za-z0-9_-]+$/.test(value.trim()) ? undefined : "A task's name, without its project"),
  });
  if (gradleTask === undefined) {
    return;
  }
  const name = gradleTask.trim();
  await context.workspaceState.update(LAST_TASK, name);
  const settings = await askSettings(context, `Profile ${name}`);
  if (settings === undefined) {
    return;
  }
  await profile(context, script, file, ["profile-run", file.fsPath], settings, name, name);
}

/** What to profile and with which options; `undefined` when dismissed. */
async function askSettings(context: vscode.ExtensionContext, title: string): Promise<ProfileSettings | undefined> {
  const last = context.workspaceState.get<ProfileSettings>(LAST_SETTINGS) ?? DEFAULT_SETTINGS;
  const events = eventsLastFirst(last.event).map((event) => ({
    label: event.label,
    description: event.name,
    detail: event.detail,
    event,
  }));
  const picked = await vscode.window.showQuickPick(events, {
    title,
    placeHolder: "What to profile",
  });
  if (picked === undefined) {
    return undefined;
  }
  const options = await vscode.window.showInputBox({
    title,
    prompt: "More async-profiler options, comma-separated: interval=1ms, alloc=512k, threads, include=app.*, …",
    placeHolder: "none",
    value: last.options,
    validateInput: refusedOptions,
  });
  if (options === undefined) {
    return undefined;
  }
  const settings: ProfileSettings = { event: picked.event.name, options: options.trim() };
  await context.workspaceState.update(LAST_SETTINGS, settings);
  return settings;
}

/**
 * Runs gradle_task.py's `scriptArguments` as a task, the choices in its
 * environment, then opens the flame graphs it wrote.
 */
async function profile(
  context: vscode.ExtensionContext,
  script: string,
  file: vscode.Uri,
  scriptArguments: string[],
  settings: ProfileSettings,
  title: string,
  gradleTask?: string,
): Promise<void> {
  const scratch = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-profile-"));
  const resultFile = path.join(scratch, "result.json");
  const execution = new vscode.ProcessExecution("python3", ["-I", script, ...scriptArguments], {
    env: profileEnvironment(settings, resultFile, gradleTask),
  });
  const definition = {
    type: PROFILE_TASK_TYPE,
    path: file.fsPath,
    event: settings.event,
    ...(settings.options ? { options: settings.options } : {}),
  };
  const scope = vscode.workspace.getWorkspaceFolder(file) ?? vscode.TaskScope.Workspace;
  const task = new vscode.Task(
    definition,
    scope,
    `profile ${settings.event}: ${title}`,
    "kotlin-analyzer",
    execution,
    [],
  );
  try {
    await runToTheEnd(task);
    const result = parseProfileResult(await fs.readFile(resultFile, "utf8").catch(() => ""));
    await show(context, result);
  } finally {
    await fs.rm(scratch, { recursive: true, force: true });
  }
}

/** Runs `task`, resolving when its process ends. */
function runToTheEnd(task: vscode.Task): Promise<void> {
  return new Promise((resolve, reject) => {
    let started: vscode.TaskExecution | undefined;
    // VS Code hands the event its own copy of the task: it is ours by name
    // and source when its execution isn't known yet.
    const ended = vscode.tasks.onDidEndTaskProcess((event) => {
      const isOurs =
        event.execution === started ||
        (event.execution.task.name === task.name && event.execution.task.source === task.source);
      if (isOurs) {
        ended.dispose();
        resolve();
      }
    });
    vscode.tasks.executeTask(task).then(
      (execution) => {
        started = execution;
      },
      (error) => {
        ended.dispose();
        reject(error);
      },
    );
  });
}

/** Each flame graph in a tab of its own; or why there is none. */
async function show(context: vscode.ExtensionContext, result: ProfileResult | undefined): Promise<void> {
  if (result === undefined || result.pages.length === 0) {
    void vscode.window.showWarningMessage(
      "kotlin-analyzer: no flame graph — no JVM was profiled (did the tests run? the terminal says why).",
    );
    return;
  }
  for (const page of result.pages) {
    const panel = vscode.window.createWebviewPanel(
      "kotlin-analyzer.flameGraph",
      flameGraphTitle(page, result.title),
      vscode.ViewColumn.Active,
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [] },
    );
    panel.webview.html = await fs.readFile(page, "utf8");
    context.subscriptions.push(panel);
  }
  const reveal = "Show the recordings";
  void vscode.window
    .showInformationMessage(`kotlin-analyzer: profiled ${result.title}.`, reveal)
    .then((chosen) => {
      if (chosen === reveal) {
        void vscode.commands.executeCommand("revealFileInOS", vscode.Uri.file(result.recordings));
      }
    });
}
