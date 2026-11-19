// VS Code's debugger for Kotlin and Java: the `kotlin-analyzer` debug type,
// run by the server's binary as `kotlin-analyzer dap`, and the command the
// "Debug test" lenses run.

import * as vscode from "vscode";

import {
  DEBUG_TESTS_COMMAND,
  DEBUG_TYPE,
  type TestRun,
  isEmptyConfiguration,
  testConfiguration,
} from "./debugging";

/** Where the adapter's binary comes from: the server's, found as it is. */
export type AdapterBinary = () => Promise<string>;

export function registerDebugger(binary: AdapterBinary): vscode.Disposable {
  const factory = vscode.debug.registerDebugAdapterDescriptorFactory(DEBUG_TYPE, {
    createDebugAdapterDescriptor: async (session) => {
      const extraEnvironment =
        vscode.workspace.getConfiguration("kotlin-analyzer").get<Record<string, string>>("server.extraEnv") ?? {};
      return new vscode.DebugAdapterExecutable(await binary(), ["dap"], {
        cwd: session.workspaceFolder?.uri.fsPath,
        env: extraEnvironment,
      });
    },
  });
  const provider = vscode.debug.registerDebugConfigurationProvider(DEBUG_TYPE, {
    // `null` opens launch.json: F5 without one has nothing to debug.
    resolveDebugConfiguration: (_folder, configuration) =>
      isEmptyConfiguration(configuration) ? null : configuration,
  });
  const debugTests = vscode.commands.registerCommand(DEBUG_TESTS_COMMAND, async (run: TestRun) => {
    const file = vscode.Uri.parse(run.uri);
    const folder = vscode.workspace.getWorkspaceFolder(file);
    await vscode.debug.startDebugging(folder, testConfiguration(file.fsPath, run.filter));
  });
  return vscode.Disposable.from(factory, provider, debugTests);
}
