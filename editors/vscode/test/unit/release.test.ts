import { strict as assert } from "node:assert";
import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { after, before, describe, it } from "node:test";
import { gzipSync } from "node:zlib";

import {
  ChecksumMismatch,
  assetUrl,
  download,
  installedBinary,
  onPath,
  targetOf,
} from "../../src/release";

describe("targetOf", () => {
  it("names the triple the release workflow builds", () => {
    assert.deepEqual(targetOf("darwin", "arm64"), { kind: "supported", triple: "aarch64-apple-darwin" });
    assert.deepEqual(targetOf("darwin", "x64"), { kind: "supported", triple: "x86_64-apple-darwin" });
    assert.deepEqual(targetOf("linux", "x64"), { kind: "supported", triple: "x86_64-unknown-linux-gnu" });
    assert.deepEqual(targetOf("linux", "arm64"), { kind: "supported", triple: "aarch64-unknown-linux-gnu" });
  });

  it("refuses what has no release", () => {
    assert.equal(targetOf("win32", "x64").kind, "unsupported");
    assert.equal(targetOf("linux", "ia32").kind, "unsupported");
  });
});

describe("assetUrl and installedBinary", () => {
  it("put the version in the tag and the directory", () => {
    assert.equal(
      assetUrl("https://example.test/releases/download", "0.4.2", "aarch64-apple-darwin"),
      "https://example.test/releases/download/v0.4.2/kotlin-analyzer-aarch64-apple-darwin.gz",
    );
    assert.equal(installedBinary("/storage", "0.4.2"), path.join("/storage", "server", "0.4.2", "kotlin-analyzer"));
  });
});

describe("download", () => {
  const binary = Buffer.from("#!/bin/sh\necho kotlin-analyzer 0.4.2\n");
  const packed = gzipSync(binary);
  const checksum = createHash("sha256").update(packed).digest("hex");
  const files = new Map<string, Buffer>([
    ["/v0.4.2/good.gz", packed],
    ["/v0.4.2/good.gz.sha256", Buffer.from(`${checksum}  good.gz\n`)],
    ["/v0.4.2/tampered.gz", packed],
    ["/v0.4.2/tampered.gz.sha256", Buffer.from(`${"0".repeat(64)}  tampered.gz\n`)],
  ]);
  let server: Server;
  let base: string;
  let directory: string;

  before(async () => {
    server = createServer((request, response) => {
      const body = files.get(request.url ?? "");
      response.writeHead(body ? 200 : 404).end(body);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    directory = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-download-"));
  });

  after(async () => {
    server.close();
    await fs.rm(directory, { recursive: true, force: true });
  });

  it("unpacks a verified binary and makes it executable", async () => {
    const destination = path.join(directory, "good", "kotlin-analyzer");
    await download(`${base}/v0.4.2/good.gz`, destination);
    assert.deepEqual(await fs.readFile(destination), binary);
    await fs.access(destination, fs.constants.X_OK);
  });

  it("keeps nothing when the checksum disagrees", async () => {
    const destination = path.join(directory, "tampered", "kotlin-analyzer");
    await assert.rejects(download(`${base}/v0.4.2/tampered.gz`, destination), ChecksumMismatch);
    await assert.rejects(fs.access(destination));
  });

  it("reports a missing asset", async () => {
    await assert.rejects(download(`${base}/v9.9.9/good.gz`, path.join(directory, "missing")), /404/);
  });
});

describe("onPath", () => {
  it("finds the first executable file and skips directories and plain files", async () => {
    const root = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-path-"));
    const asDirectory = path.join(root, "first");
    const notExecutable = path.join(root, "second");
    const executable = path.join(root, "third");
    await fs.mkdir(path.join(asDirectory, "kotlin-analyzer"), { recursive: true });
    await fs.mkdir(notExecutable);
    await fs.writeFile(path.join(notExecutable, "kotlin-analyzer"), "", { mode: 0o644 });
    await fs.mkdir(executable);
    await fs.writeFile(path.join(executable, "kotlin-analyzer"), "", { mode: 0o755 });
    const searchPath = ["", asDirectory, notExecutable, executable].join(path.delimiter);
    assert.equal(await onPath("kotlin-analyzer", searchPath), path.join(executable, "kotlin-analyzer"));
    assert.equal(await onPath("kotlin-analyzer", asDirectory), undefined);
    await fs.rm(root, { recursive: true, force: true });
  });
});
