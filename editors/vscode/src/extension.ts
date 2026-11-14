// Starts kotlin-analyzer for the workspace's Kotlin (and Java) files and
// keeps it running: restarted when a setting it reads only at start changes.

import * as vscode from "vscode";
import {
  LanguageClient,
  type DocumentSelector,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import { findServer, type ServerBinary } from "./server";
import { RESTART_SETTINGS, initializationOptions } from "./settings";

const SECTION = "kotlin-analyzer";

/** What the extension hands to other extensions (and its tests). */
export interface Api {
  /** The running client, once started. */
  client(): LanguageClient | undefined;
  /** The binary the client runs, and where it came from. */
  binary(): ServerBinary | undefined;
}

let client: LanguageClient | undefined;
let binary: ServerBinary | undefined;
let output: vscode.LogOutputChannel | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<Api> {
  output = vscode.window.createOutputChannel("kotlin-analyzer", { log: true });
  context.subscriptions.push(
    output,
    vscode.commands.registerCommand(`${SECTION}.restartServer`, () => restart(context)),
    vscode.commands.registerCommand(`${SECTION}.showLogs`, () => output?.show()),
    vscode.workspace.onDidChangeConfiguration((change) => {
      const needsRestart = RESTART_SETTINGS.some((setting) => change.affectsConfiguration(setting));
      if (needsRestart) {
        void restart(context);
      }
    }),
  );
  await start(context);
  return { client: () => client, binary: () => binary };
}

export async function deactivate(): Promise<void> {
  await stop();
}

async function start(context: vscode.ExtensionContext): Promise<void> {
  const configuration = vscode.workspace.getConfiguration(SECTION);
  binary = undefined;
  try {
    binary = await findServer(context);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    output?.error(`Not started: ${message}`);
    void vscode.window.showErrorMessage(`kotlin-analyzer: ${message}`);
    return;
  }
  output?.info(`Starting ${binary.path} (from the ${binary.source})`);
  const extraEnvironment = configuration.get<Record<string, string>>("server.extraEnv") ?? {};
  const serverOptions: ServerOptions = {
    command: binary.path,
    options: { env: { ...process.env, ...extraEnvironment } },
  };
  const selector: DocumentSelector = [{ scheme: "file", language: "kotlin" }];
  if (configuration.get<boolean>("java.enable") ?? true) {
    selector.push({ scheme: "file", language: "java" });
  }
  const clientOptions: LanguageClientOptions = {
    documentSelector: selector,
    initializationOptions: initializationOptions(configuration),
    synchronize: { configurationSection: SECTION },
    outputChannel: output,
  };
  client = new LanguageClient(SECTION, "kotlin-analyzer", serverOptions, clientOptions);
  await client.start();
}

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  if (running?.isRunning()) {
    await running.stop();
  }
}

async function restart(context: vscode.ExtensionContext): Promise<void> {
  await stop();
  await start(context);
}
