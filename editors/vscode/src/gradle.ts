// Gradle tasks for the active file's build, run by gradle_task.py (bundled
// as dist/gradle_task.py): build without tests, clean, and test — one test
// task for the file's project and one for each parent, up to the build.

import { execFile } from "node:child_process";
import * as path from "node:path";
import { promisify } from "node:util";
import * as vscode from "vscode";

export const TASK_TYPE = "kotlin-gradle";

type GradleTask = "build" | "clean" | "test";

interface GradleTaskDefinition extends vscode.TaskDefinition {
  task: GradleTask;
  /** A file or directory in the build; for `test`, in the project to test. */
  path: string;
}

/** A project holding the file, as gradle_task.py lists it. */
interface Project {
  path: string;
  directory: string;
}

export function registerGradleTasks(context: vscode.ExtensionContext): vscode.Disposable {
  const script = context.asAbsolutePath(path.join("dist", "gradle_task.py"));
  return vscode.tasks.registerTaskProvider(TASK_TYPE, {
    provideTasks: () => tasksForActiveFile(script),
    resolveTask: (task) => {
      const definition = task.definition as GradleTaskDefinition;
      return gradleTask(script, definition, nameOf(definition, undefined), task.scope);
    },
  });
}

async function tasksForActiveFile(script: string): Promise<vscode.Task[]> {
  const target = vscode.window.activeTextEditor?.document.uri ?? vscode.workspace.workspaceFolders?.[0]?.uri;
  if (target?.scheme !== "file") {
    return [];
  }
  const projects = await projectsHolding(script, target.fsPath);
  if (projects.length === 0) {
    return [];
  }
  const scope = vscode.workspace.getWorkspaceFolder(target) ?? vscode.TaskScope.Workspace;
  const whole = (task: GradleTask): GradleTaskDefinition => ({ type: TASK_TYPE, task, path: target.fsPath });
  const tests = projects.map((project) => {
    const definition: GradleTaskDefinition = { type: TASK_TYPE, task: "test", path: project.directory };
    return gradleTask(script, definition, nameOf(definition, project), scope);
  });
  return [
    gradleTask(script, whole("build"), nameOf(whole("build"), undefined), scope),
    gradleTask(script, whole("clean"), nameOf(whole("clean"), undefined), scope),
    ...tests,
  ];
}

function nameOf(definition: GradleTaskDefinition, project: Project | undefined): string {
  switch (definition.task) {
    case "build":
      return "gradle build -x test";
    case "clean":
      return "gradle clean";
    case "test":
      return `gradle test --rerun-tasks: ${project?.path ?? path.basename(definition.path)}`;
  }
}

function gradleTask(
  script: string,
  definition: GradleTaskDefinition,
  name: string,
  scope: vscode.WorkspaceFolder | vscode.TaskScope | undefined,
): vscode.Task {
  const execution = new vscode.ProcessExecution("python3", ["-I", script, definition.task, definition.path]);
  const task = new vscode.Task(definition, scope ?? vscode.TaskScope.Workspace, name, "kotlin-analyzer", execution, []);
  task.group = { build: vscode.TaskGroup.Build, clean: vscode.TaskGroup.Clean, test: vscode.TaskGroup.Test }[
    definition.task
  ];
  return task;
}

/** The projects holding `file`, nearest first; none outside a Gradle build. */
async function projectsHolding(script: string, file: string): Promise<Project[]> {
  try {
    const { stdout } = await promisify(execFile)("python3", ["-I", script, "projects", file]);
    return JSON.parse(stdout) as Project[];
  } catch {
    return [];
  }
}
