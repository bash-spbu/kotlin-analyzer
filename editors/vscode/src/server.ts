// Which server binary to run: the configured one, else the one on the PATH,
// else this extension's release, downloaded once into the global storage.
// The formatter the server runs is found the same way.

import * as vscode from "vscode";
import { existsSync } from "node:fs";

import { downloadFormatter, formatterAsset, formatterOf, installedProgram } from "./formatter";
import {
  RELEASES,
  assetUrl,
  download,
  installedBinary,
  onPath,
  targetOf,
} from "./release";

/** Where the binary came from, so errors can say what to change. */
export type ServerBinary =
  | { source: "setting"; path: string }
  | { source: "path"; path: string }
  | { source: "release"; path: string };

export async function findServer(context: vscode.ExtensionContext): Promise<ServerBinary> {
  const configured = vscode.workspace.getConfiguration("kotlin-analyzer").get<string | null>("server.path");
  if (configured) {
    if (!existsSync(configured)) {
      throw new Error(`kotlin-analyzer.server.path is ${configured}, which doesn't exist`);
    }
    return { source: "setting", path: configured };
  }
  const found = await onPath("kotlin-analyzer", process.env.PATH);
  if (found !== undefined) {
    return { source: "path", path: found };
  }
  return { source: "release", path: await releaseBinary(context) };
}

async function releaseBinary(context: vscode.ExtensionContext): Promise<string> {
  const version: string = context.extension.packageJSON.version;
  const binary = installedBinary(context.globalStorageUri.fsPath, version);
  if (existsSync(binary)) {
    return binary;
  }
  const target = targetOf(process.platform, process.arch);
  if (target.kind === "unsupported") {
    throw new Error(
      `${target.reason}: build it (cargo install --path crates/kotlin-analyzer) and set kotlin-analyzer.server.path`,
    );
  }
  const downloads = process.env.KOTLIN_ANALYZER_DOWNLOADS ?? RELEASES;
  const url = assetUrl(downloads, version, target.triple);
  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: `Downloading kotlin-analyzer ${version}`,
    },
    () => download(url, binary),
  );
  return binary;
}

/**
 * The formatter the settings choose: `formatting.path` when set (the server
 * takes it as it is), else the formatter on the PATH, else its pinned
 * release, downloaded once into the global storage. `undefined` when
 * formatting is off or the download failed: the server then looks on its
 * PATH, and says so when it formats.
 */
export async function findFormatter(
  context: vscode.ExtensionContext,
  output: vscode.LogOutputChannel | undefined,
): Promise<string | undefined> {
  const configuration = vscode.workspace.getConfiguration("kotlin-analyzer");
  const formatter = formatterOf(configuration.get<string>("formatting.formatter"));
  const configured = configuration.get<string | null>("formatting.path");
  if (formatter === undefined || configured) {
    return undefined;
  }
  const found = await onPath(formatter, process.env.PATH);
  if (found !== undefined) {
    return found;
  }
  const asset = formatterAsset(formatter, process.platform, process.arch);
  if (asset === undefined) {
    return undefined;
  }
  const program = installedProgram(context.globalStorageUri.fsPath, asset);
  if (existsSync(program)) {
    return program;
  }
  try {
    await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `Downloading ${asset.fileName}` },
      () => downloadFormatter(asset, program),
    );
    return program;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output?.error(`Downloading ${formatter}: ${message}`);
    void vscode.window.showWarningMessage(`kotlin-analyzer: couldn't download ${formatter}: ${message}`);
    return undefined;
  }
}
