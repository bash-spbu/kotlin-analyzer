// The server's GitHub releases: which asset fits this machine, and fetching
// it with its checksum verified. No `vscode` here, so it runs under plain
// Node in the unit tests.

import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import { gunzipSync } from "node:zlib";

export const RELEASES = "https://github.com/bash-spbu/kotlin-analyzer/releases/download";

/** A Rust target triple the release workflow builds, or why there's none. */
export type Target =
  | { kind: "supported"; triple: string }
  | { kind: "unsupported"; reason: string };

export function targetOf(platform: NodeJS.Platform, architecture: string): Target {
  const systems: Partial<Record<NodeJS.Platform, string>> = {
    darwin: "apple-darwin",
    linux: "unknown-linux-gnu",
  };
  const architectures: Record<string, string> = { arm64: "aarch64", x64: "x86_64" };
  const system = systems[platform];
  const machine = architectures[architecture];
  if (system === undefined) {
    return { kind: "unsupported", reason: `no kotlin-analyzer release for ${platform}` };
  }
  if (machine === undefined) {
    return { kind: "unsupported", reason: `no kotlin-analyzer release for ${architecture}` };
  }
  return { kind: "supported", triple: `${machine}-${system}` };
}

export function assetName(triple: string): string {
  return `kotlin-analyzer-${triple}.gz`;
}

/** Where release `version`'s asset for `triple` is downloaded from. */
export function assetUrl(downloads: string, version: string, triple: string): string {
  return `${downloads}/v${version}/${assetName(triple)}`;
}

/** Where a downloaded release lives: one directory per version. */
export function installedBinary(storage: string, version: string): string {
  return path.join(storage, "server", version, "kotlin-analyzer");
}

export class ChecksumMismatch extends Error {
  constructor(asset: string, expected: string, actual: string) {
    super(`${asset} has SHA-256 ${actual}, the release says ${expected}`);
  }
}

/**
 * Downloads `url` and `url.sha256`, checks the one against the other,
 * unpacks the binary to `destination` (made executable). A partial download
 * never takes `destination`'s place.
 */
export async function download(url: string, destination: string): Promise<void> {
  const packed = Buffer.from(await fetchBytes(url));
  const checksumFile = Buffer.from(await fetchBytes(`${url}.sha256`)).toString("utf8");
  const expected = checksumFile.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  const actual = createHash("sha256").update(packed).digest("hex");
  if (expected !== actual) {
    throw new ChecksumMismatch(path.basename(url), expected, actual);
  }
  await fs.mkdir(path.dirname(destination), { recursive: true });
  const partial = `${destination}.partial`;
  await fs.writeFile(partial, gunzipSync(packed), { mode: 0o755 });
  await fs.rename(partial, destination);
}

export async function fetchBytes(url: string): Promise<ArrayBuffer> {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`GET ${url}: ${response.status} ${response.statusText}`);
  }
  return response.arrayBuffer();
}

/** The first `name` in `searchPath` (a `PATH` value) that is executable. */
export async function onPath(name: string, searchPath: string | undefined): Promise<string | undefined> {
  for (const directory of (searchPath ?? "").split(path.delimiter)) {
    if (directory === "") {
      continue;
    }
    const candidate = path.join(directory, name);
    const executable = await fs.access(candidate, fs.constants.X_OK).then(
      () => true,
      () => false,
    );
    const isFile = executable && (await fs.stat(candidate)).isFile();
    if (isFile) {
      return candidate;
    }
  }
  return undefined;
}
