// Which server binary to run: the configured one, else the one on the PATH,
// else this extension's release, downloaded once into the global storage.

import * as vscode from "vscode";
import { existsSync } from "node:fs";

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
