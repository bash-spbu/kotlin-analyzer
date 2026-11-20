// The formatters the server runs (`kotlin-analyzer.formatting.formatter`):
// ktrs, the default, and ktfmt. Each is downloaded at a pinned version whose
// SHA-256 is written here (the same as in editors/zed/src/formatter.rs), and
// taken out of its archive only once that agrees. No `vscode` here, so it
// runs under plain Node in the unit tests.

import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import * as path from "node:path";
import { gunzipSync } from "node:zlib";

import { ChecksumMismatch, fetchBytes } from "./release";

export const KTRS_VERSION = "0.5.3";
export const KTFMT_VERSION = "0.65";

export type Formatter = "ktrs" | "ktfmt";

/** The setting's formatter: `ktrs` unless it names `ktfmt`; none for `off`. */
export function formatterOf(setting: string | undefined): Formatter | undefined {
  switch (setting) {
    case "off":
      return undefined;
    case "ktfmt":
      return "ktfmt";
    default:
      return "ktrs";
  }
}

/** How a download holds the program. */
export type Packaging =
  | { kind: "tar.gz"; member: string }
  /** The program itself: ktfmt's jar, which the server runs with `java`. */
  | { kind: "jar" };

/** One formatter's download for one machine. */
export interface FormatterAsset {
  formatter: Formatter;
  url: string;
  fileName: string;
  sha256: string;
  packaging: Packaging;
}

const KTRS_CHECKSUMS: Record<string, string> = {
  "aarch64-apple-darwin": "5dd74fe28b4d14470ec3408675681300146507e701271c37f31152066d7fdb4c",
  "x86_64-apple-darwin": "59c5931f05893686b9f4b7c63313a5ec78d4c16e6e3fd2204a248ceb31017bd8",
  "aarch64-unknown-linux-musl": "2e52c6c85a1800f4ca20651abd7e9ff2d67ebe7041dd5b2355028af629ef745d",
  "x86_64-unknown-linux-musl": "80913a1e65f8821797d2c7fca3b1f82ca8d1b8c59cbc2e00e8024691608afe62",
};

/** ktfmt's native builds; elsewhere, its jar. */
const KTFMT_NATIVE_CHECKSUMS: Record<string, string> = {
  "macos-aarch64": "d10ec46dadc4ae19e5b8ddc72d10ce45e5be35d632ce9e73c9b8a59d2147fb85",
  "linux-x86_64": "3684327b092db9b76fffcb9525decf73f8331ed17e00ce3232769c11cd36d352",
};
const KTFMT_JAR_CHECKSUM = "aba9753b6ab387926179ab7318ba8e0c33dad483ebc72f633e79522265b15214";

/** The formatter's download for this machine, or `undefined` where there's none. */
export function formatterAsset(
  formatter: Formatter,
  platform: NodeJS.Platform,
  architecture: string,
): FormatterAsset | undefined {
  const systems: Partial<Record<NodeJS.Platform, { ktrs: string; ktfmt: string }>> = {
    darwin: { ktrs: "apple-darwin", ktfmt: "macos" },
    linux: { ktrs: "unknown-linux-musl", ktfmt: "linux" },
  };
  const machines: Record<string, string> = { arm64: "aarch64", x64: "x86_64" };
  const system = systems[platform];
  const machine = machines[architecture];
  if (system === undefined || machine === undefined) {
    return undefined;
  }
  if (formatter === "ktrs") {
    const triple = `${machine}-${system.ktrs}`;
    const stem = `ktrs-v${KTRS_VERSION}-${triple}`;
    const fileName = `${stem}.tar.gz`;
    return {
      formatter,
      url: `https://github.com/Hexay/ktrs/releases/download/v${KTRS_VERSION}/${fileName}`,
      fileName,
      sha256: KTRS_CHECKSUMS[triple],
      packaging: { kind: "tar.gz", member: `${stem}/ktrs` },
    };
  }
  const release = `https://github.com/facebook/ktfmt/releases/download/v${KTFMT_VERSION}`;
  const platformName = `${system.ktfmt}-${machine}`;
  const nativeChecksum = KTFMT_NATIVE_CHECKSUMS[platformName];
  if (nativeChecksum === undefined) {
    const fileName = `ktfmt-${KTFMT_VERSION}-with-dependencies.jar`;
    return {
      formatter,
      url: `${release}/${fileName}`,
      fileName,
      sha256: KTFMT_JAR_CHECKSUM,
      packaging: { kind: "jar" },
    };
  }
  const stem = `ktfmt-${platformName}-${KTFMT_VERSION}`;
  const fileName = `${stem}.tar.gz`;
  return {
    formatter,
    url: `${release}/${fileName}`,
    fileName,
    sha256: nativeChecksum,
    packaging: { kind: "tar.gz", member: `${stem}/ktfmt` },
  };
}

/** Where a downloaded formatter's program lives: one directory per version. */
export function installedProgram(storage: string, asset: FormatterAsset): string {
  const version = asset.formatter === "ktrs" ? KTRS_VERSION : KTFMT_VERSION;
  const file = asset.packaging.kind === "jar" ? asset.fileName : asset.formatter;
  return path.join(storage, "formatters", `${asset.formatter}-${version}`, file);
}

/** The program out of `downloaded`, if its SHA-256 is the pinned one. */
export function verifiedProgram(asset: FormatterAsset, downloaded: Buffer): Buffer {
  const actual = createHash("sha256").update(downloaded).digest("hex");
  if (actual !== asset.sha256) {
    throw new ChecksumMismatch(asset.fileName, asset.sha256, actual);
  }
  if (asset.packaging.kind === "jar") {
    return downloaded;
  }
  const member = tarMember(gunzipSync(downloaded), asset.packaging.member);
  if (member === undefined) {
    throw new Error(`${asset.fileName} has no ${asset.packaging.member}`);
  }
  return member;
}

/**
 * Downloads `asset`, and writes its program to `destination` (executable)
 * only once its checksum is the pinned one. A partial download never takes
 * `destination`'s place.
 */
export async function downloadFormatter(asset: FormatterAsset, destination: string): Promise<void> {
  const program = verifiedProgram(asset, Buffer.from(await fetchBytes(asset.url)));
  await fs.mkdir(path.dirname(destination), { recursive: true });
  const partial = `${destination}.partial`;
  await fs.writeFile(partial, program, { mode: 0o755 });
  await fs.rename(partial, destination);
}

const BLOCK = 512;

/**
 * The file at `member` in the (uncompressed) tar `archive`. A name is a
 * ustar header's (up to 100 bytes, or 255 with the prefix field), or the
 * long one a record before it gives: GNU tar's `././@LongLink` (type `L`)
 * or a pax header's `path` (type `x`).
 */
export function tarMember(archive: Buffer, member: string): Buffer | undefined {
  let offset = 0;
  let longName: string | undefined;
  while (offset + BLOCK <= archive.length) {
    const header = archive.subarray(offset, offset + BLOCK);
    const isEnd = header.every((byte) => byte === 0);
    if (isEnd) {
      return undefined;
    }
    const size = parseInt(field(header, 124, 12).trim() || "0", 8);
    const type = String.fromCharCode(header[156]);
    const dataStart = offset + BLOCK;
    const data = archive.subarray(dataStart, dataStart + size);
    offset = dataStart + Math.ceil(size / BLOCK) * BLOCK;
    if (type === "L") {
      longName = field(data, 0, data.length);
      continue;
    }
    if (type === "x") {
      longName = paxPath(data) ?? longName;
      continue;
    }
    const name = longName ?? ustarName(header);
    longName = undefined;
    const isFile = type === "0" || type === "\0";
    if (isFile && name === member) {
      return data;
    }
  }
  return undefined;
}

/** A ustar header's name: its prefix field, if any, then its name field. */
function ustarName(header: Buffer): string {
  const prefix = field(header, 345, 155);
  const name = field(header, 0, 100);
  return prefix === "" ? name : `${prefix}/${name}`;
}

/** The `path` of a pax extended header's records (`<length> path=<value>\n`). */
function paxPath(records: Buffer): string | undefined {
  let offset = 0;
  while (offset < records.length) {
    const space = records.indexOf(0x20, offset);
    const length = parseInt(records.subarray(offset, space).toString("utf8"), 10);
    const isMalformed = space === -1 || !(length > 0);
    if (isMalformed) {
      return undefined;
    }
    const record = records.subarray(space + 1, offset + length - 1).toString("utf8");
    const equals = record.indexOf("=");
    if (record.slice(0, equals) === "path") {
      return record.slice(equals + 1);
    }
    offset += length;
  }
  return undefined;
}

/** A NUL-terminated text field of a tar header. */
function field(header: Buffer, start: number, length: number): string {
  const bytes = header.subarray(start, start + length);
  const end = bytes.indexOf(0);
  return bytes.subarray(0, end === -1 ? length : end).toString("utf8");
}
