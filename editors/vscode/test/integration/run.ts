// Runs the extension in a real VS Code against a fresh Kotlin workspace,
// with no kotlin-analyzer on the PATH: the extension has to download its
// release — served here from the locally built binary — and verify it. The
// formatter is a script set in the workspace's settings (the pinned
// downloads are checked in the unit tests).
//
//   KOTLIN_ANALYZER_BINARY  the server to serve as the release (default:
//                           ../../target/release/kotlin-analyzer)
//   VSCODE_EXECUTABLE       a VS Code to run (default: download one)

import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { gzipSync } from "node:zlib";
import { runTests } from "@vscode/test-electron";

import { assetName, targetOf } from "../../src/release";

const extensionRoot = path.resolve(__dirname, "../../..");

async function main(): Promise<void> {
  const target = targetOf(process.platform, process.arch);
  if (target.kind === "unsupported") {
    throw new Error(target.reason);
  }
  const version: string = JSON.parse(await fs.readFile(path.join(extensionRoot, "package.json"), "utf8")).version;
  const binary =
    process.env.KOTLIN_ANALYZER_BINARY ?? path.resolve(extensionRoot, "../../target/release/kotlin-analyzer");
  const packed = gzipSync(await fs.readFile(binary));
  const checksum = createHash("sha256").update(packed).digest("hex");
  const asset = assetName(target.triple);
  const release = new Map<string, Buffer>([
    [`/v${version}/${asset}`, packed],
    [`/v${version}/${asset}.sha256`, Buffer.from(`${checksum}  ${asset}\n`)],
  ]);
  const server = createServer((request, response) => {
    const body = release.get(request.url ?? "");
    response.writeHead(body ? 200 : 404).end(body);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const downloads = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;

  const scratch = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-vscode-"));
  const workspace = path.join(scratch, "workspace");
  await writeWorkspace(workspace);
  // VS Code passes its own environment on to the extension host, PATH
  // included, so the PATH without the server goes to VS Code itself — which
  // mustn't add the login shell's (--force-disable-user-env).
  process.env.PATH = "/usr/bin:/bin:/usr/sbin:/sbin";
  // The Gradle tasks run in VS Code's terminals, whose environment is VS
  // Code's: the recording gradlew writes its call here.
  process.env.GRADLE_CALL = path.join(scratch, "gradle-call");
  try {
    await runTests({
      vscodeExecutablePath: process.env.VSCODE_EXECUTABLE,
      extensionDevelopmentPath: extensionRoot,
      extensionTestsPath: path.join(__dirname, "suite.js"),
      launchArgs: [
        workspace,
        "--disable-extensions",
        "--force-disable-user-env",
        `--user-data-dir=${path.join(scratch, "user-data")}`,
        `--extensions-dir=${path.join(scratch, "extensions")}`,
      ],
      extensionTestsEnv: {
        KOTLIN_ANALYZER_DOWNLOADS: downloads,
        PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
        KOTLIN_ANALYZER_TEST_WORKSPACE: workspace,
        GRADLE_CALL: path.join(scratch, "gradle-call"),
      },
    });
  } finally {
    server.close();
    await fs.rm(scratch, { recursive: true, force: true });
  }
}

async function writeWorkspace(root: string): Promise<void> {
  const sources = path.join(root, "src/main/kotlin/app");
  await fs.mkdir(sources, { recursive: true });
  await fs.mkdir(path.join(root, ".vscode"));
  await fs.writeFile(
    path.join(root, ".vscode/settings.json"),
    JSON.stringify({
      "kotlin-analyzer.gradleExport": false,
      "kotlin-analyzer.formatting.path": path.join(root, "fake-ktrs"),
    }),
  );
  // Formats like ktrs would, for `Format.kt`; says how it was called.
  await fs.writeFile(
    path.join(root, "fake-ktrs"),
    '#!/bin/sh\necho "$@" > "$0.call"\nsed "s/val one=1/val one = 1/"\n',
    { mode: 0o755 },
  );
  await fs.writeFile(path.join(sources, "Format.kt"), "package app\n\nval one=1\n");
  await fs.writeFile(path.join(sources, "Box.kt"), "package app\n\nclass Box(val size: Int)\n");
  await fs.writeFile(path.join(sources, "Use.kt"), "package app\n\nfun use(): Int = Box(3).size\n");
  await fs.writeFile(path.join(sources, "Broken.kt"), "package app\n\nfun broken( {\n");
  await fs.writeFile(path.join(root, "settings.gradle.kts"), 'rootProject.name = "demo"\ninclude("lib")\n');
  await fs.writeFile(path.join(root, "build.gradle.kts"), "");
  await fs.mkdir(path.join(root, "lib/src/main/kotlin/lib"), { recursive: true });
  await fs.writeFile(path.join(root, "lib/build.gradle.kts"), "");
  await fs.writeFile(path.join(root, "lib/src/main/kotlin/lib/Lib.kt"), "package lib\n\nclass Lib\n");
  await fs.mkdir(path.join(root, "lib/src/test/kotlin/lib"), { recursive: true });
  await fs.writeFile(
    path.join(root, "lib/src/test/kotlin/lib/LibTest.kt"),
    "package lib\n\nimport org.junit.jupiter.api.Test\n\nclass LibTest {\n    @Test\n    fun works() { Lib() }\n}\n",
  );
  // Records where and how it was called instead of running Gradle.
  await fs.writeFile(
    path.join(root, "gradlew"),
    '#!/bin/sh\n{ pwd; for argument in "$@"; do echo "$argument"; done; } > "$GRADLE_CALL"\n',
    { mode: 0o755 },
  );
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
