// The extension's settings as the server reads them (docs/spec/lsp.md):
// `initializationOptions` at start, `diagnostics` again on every change.

import * as vscode from "vscode";

export interface InitializationOptions {
  diagnostics: { disabled: string[]; experimental: boolean };
  gradleExport: boolean;
  projectJson?: string;
}

export function initializationOptions(
  configuration: vscode.WorkspaceConfiguration,
): InitializationOptions {
  const projectJson = configuration.get<string | null>("projectJson");
  return {
    diagnostics: {
      disabled: configuration.get<string[]>("diagnostics.disabled") ?? [],
      experimental: configuration.get<boolean>("diagnostics.experimental") ?? false,
    },
    gradleExport: configuration.get<boolean>("gradleExport") ?? true,
    ...(projectJson ? { projectJson } : {}),
  };
}

/** The settings only a restart of the server applies. */
export const RESTART_SETTINGS = [
  "kotlin-analyzer.server",
  "kotlin-analyzer.java.enable",
  "kotlin-analyzer.gradleExport",
  "kotlin-analyzer.projectJson",
];
