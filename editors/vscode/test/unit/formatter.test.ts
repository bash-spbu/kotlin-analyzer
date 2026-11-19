import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { after, before, describe, it } from "node:test";
import { gunzipSync } from "node:zlib";

import {
  downloadFormatter,
  formatterAsset,
  formatterOf,
  installedProgram,
  tarMember,
  type FormatterAsset,
} from "../../src/formatter";
import { ChecksumMismatch } from "../../src/release";

/** A `.tar.gz` of `files` (relative paths), made by the system's `tar`. */
async function tarGz(files: Record<string, string>): Promise<Buffer> {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-tar-"));
  for (const [name, text] of Object.entries(files)) {
    await fs.mkdir(path.dirname(path.join(root, name)), { recursive: true });
    await fs.writeFile(path.join(root, name), text, { mode: 0o755 });
  }
  const archive = execFileSync("tar", ["-czf", "-", "-C", root, ...Object.keys(files)]);
  await fs.rm(root, { recursive: true, force: true });
  return archive;
}

function sha256(bytes: Buffer): string {
  return createHash("sha256").update(bytes).digest("hex");
}

describe("formatterOf", () => {
  it("picks ktrs unless the setting says ktfmt or off", () => {
    assert.equal(formatterOf(undefined), "ktrs");
    assert.equal(formatterOf("ktfmt"), "ktfmt");
    assert.equal(formatterOf("prettier"), "ktrs");
    assert.equal(formatterOf("off"), undefined);
  });
});

describe("formatterAsset", () => {
  it("is ktrs's release archive, musl on Linux", () => {
    const asset = formatterAsset("ktrs", "linux", "arm64");
    assert.equal(
      asset?.url,
      "https://github.com/Hexay/ktrs/releases/download/v0.5.3/ktrs-v0.5.3-aarch64-unknown-linux-musl.tar.gz",
    );
    assert.deepEqual(asset?.packaging, { kind: "tar.gz", member: "ktrs-v0.5.3-aarch64-unknown-linux-musl/ktrs" });
    assert.equal(
      installedProgram("/storage", asset!),
      path.join("/storage", "formatters", "ktrs-0.5.3", "ktrs"),
    );
  });

  it("is ktfmt's native build where there is one and its jar elsewhere", () => {
    const native = formatterAsset("ktfmt", "darwin", "arm64");
    assert.equal(
      native?.url,
      "https://github.com/facebook/ktfmt/releases/download/v0.65/ktfmt-macos-aarch64-0.65.tar.gz",
    );
    const jar = formatterAsset("ktfmt", "darwin", "x64");
    assert.deepEqual(jar?.packaging, { kind: "jar" });
    assert.equal(
      installedProgram("/storage", jar!),
      path.join("/storage", "formatters", "ktfmt-0.65", "ktfmt-0.65-with-dependencies.jar"),
    );
    assert.equal(formatterAsset("ktrs", "win32", "x64"), undefined);
  });
});

describe("tarMember", () => {
  it("finds a file by its whole path, a long one too", async () => {
    const long = `${"directory-".repeat(12)}/ktrs`;
    const archive = gunzipSync(
      await tarGz({ "ktrs-v0.5.3/README.md": "readme", "ktrs-v0.5.3/ktrs": "the program", [long]: "deep" }),
    );
    assert.equal(tarMember(archive, "ktrs-v0.5.3/ktrs")?.toString(), "the program");
    assert.equal(tarMember(archive, long)?.toString(), "deep");
    assert.equal(tarMember(archive, "ktrs-v0.5.3/ktfmt"), undefined);
  });
});

describe("downloadFormatter", () => {
  let server: Server;
  let base: string;
  let directory: string;
  let packed: Buffer;

  before(async () => {
    packed = await tarGz({ "ktrs-v0.5.3-x86_64-apple-darwin/ktrs": "#!/bin/sh\ncat\n" });
    server = createServer((request, response) => {
      const body = request.url === "/ktrs.tar.gz" ? packed : undefined;
      response.writeHead(body ? 200 : 404).end(body);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    directory = await fs.mkdtemp(path.join(os.tmpdir(), "kotlin-analyzer-formatter-"));
  });

  after(async () => {
    server.close();
    await fs.rm(directory, { recursive: true, force: true });
  });

  function asset(sha256Hex: string): FormatterAsset {
    return {
      ...formatterAsset("ktrs", "darwin", "x64")!,
      url: `${base}/ktrs.tar.gz`,
      sha256: sha256Hex,
    };
  }

  it("writes the program out of an archive with the pinned checksum, executable", async () => {
    const destination = path.join(directory, "good", "ktrs");
    await downloadFormatter(asset(sha256(packed)), destination);
    assert.equal(await fs.readFile(destination, "utf8"), "#!/bin/sh\ncat\n");
    await fs.access(destination, fs.constants.X_OK);
  });

  it("keeps nothing when the checksum isn't the pinned one", async () => {
    const destination = path.join(directory, "tampered", "ktrs");
    await assert.rejects(downloadFormatter(asset("0".repeat(64)), destination), ChecksumMismatch);
    await assert.rejects(fs.access(destination));
  });
});
