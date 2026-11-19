// Starts kotlin-analyzer for the workspace's Kotlin (and Java) files and
// keeps it running: restarted when a setting it reads only at start changes.

import * as vscode from "vscode";
import {
  DidChangeConfigurationNotification,
  LanguageClient,
  type DocumentSelector,
  type Location,
  type Position,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import { registerGradleTasks } from "./gradle";
import { findFormatter, findServer, type ServerBinary } from "./server";
import { RESTART_SETTINGS, initializationOptions, serverSettings } from "./settings";

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
    registerGradleTasks(context),
    vscode.commands.registerCommand(SHOW_REFERENCES_COMMAND, showReferences),
    vscode.workspace.onDidChangeConfiguration((change) => {
      const needsRestart = RESTART_SETTINGS.some((setting) => change.affectsConfiguration(setting));
      if (needsRestart) {
        void restart(context);
      } else if (change.affectsConfiguration(SECTION)) {
        void sendSettings(context);
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
  const formatterPath = await findFormatter(context, output);
  const clientOptions: LanguageClientOptions = {
    documentSelector: selector,
    initializationOptions: initializationOptions(configuration, formatterPath),
    outputChannel: output,
  };
  client = new LanguageClient(SECTION, "kotlin-analyzer", serverOptions, clientOptions);
  await client.start();
}

/** The command of the server's reference-count lenses (docs/spec/lsp.md). */
const SHOW_REFERENCES_COMMAND = `${SECTION}.showReferences`;

/** The references a lens counted, in VS Code's peek view. */
async function showReferences(uri: string, position: Position, locations: Location[]): Promise<void> {
  const converter = client?.protocol2CodeConverter;
  if (converter === undefined) {
    return;
  }
  await vscode.commands.executeCommand(
    "editor.action.showReferences",
    converter.asUri(uri),
    converter.asPosition(position),
    locations.map((location) => converter.asLocation(location)),
  );
}

/**
 * The changed settings to the running server, with the formatter they now
 * choose (downloaded first if need be).
 */
async function sendSettings(context: vscode.ExtensionContext): Promise<void> {
  const formatterPath = await findFormatter(context, output);
  const settings = serverSettings(vscode.workspace.getConfiguration(SECTION), formatterPath);
  if (client?.isRunning()) {
    await client.sendNotification(DidChangeConfigurationNotification.type, {
      settings: { [SECTION]: settings },
    });
  }
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
