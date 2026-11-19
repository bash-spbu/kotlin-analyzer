// The extension's settings as the server reads them (docs/spec/lsp.md):
// `initializationOptions` at start; `diagnostics` and `formatting` again,
// under `kotlin-analyzer`, on every change.

import * as vscode from "vscode";

export interface ServerSettings {
  diagnostics: { disabled: string[]; experimental: boolean };
  formatting: { formatter: string; style: string; path?: string };
}

export interface InitializationOptions extends ServerSettings {
  gradleExport: boolean;
  projectJson?: string;
}

/**
 * The settings the server follows as they change, with `formatterPath` (the
 * formatter found or downloaded) where `formatting.path` isn't set.
 */
export function serverSettings(
  configuration: vscode.WorkspaceConfiguration,
  formatterPath: string | undefined,
): ServerSettings {
  const path = configuration.get<string | null>("formatting.path") || formatterPath;
  return {
    diagnostics: {
      disabled: configuration.get<string[]>("diagnostics.disabled") ?? [],
      experimental: configuration.get<boolean>("diagnostics.experimental") ?? false,
    },
    formatting: {
      formatter: configuration.get<string>("formatting.formatter") ?? "ktrs",
      style: configuration.get<string>("formatting.style") ?? "kotlinlang",
      ...(path ? { path } : {}),
    },
  };
}

export function initializationOptions(
  configuration: vscode.WorkspaceConfiguration,
  formatterPath: string | undefined,
): InitializationOptions {
  const projectJson = configuration.get<string | null>("projectJson");
  return {
    ...serverSettings(configuration, formatterPath),
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
