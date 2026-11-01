#!/usr/bin/env node
// Builds `s08-sidecar` and copies it to `src-tauri/binaries/` with the
// target-triple suffix that Tauri's `bundle.externalBin` expects, for
// example `s08-sidecar-aarch64-apple-darwin`.
// Child processes always run with an argument array, never a shell string.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import process from "node:process";

// The spike root, independent of the current working directory.
const root = path.resolve(fileURLToPath(new URL("..", import.meta.url)));

// The host triple comes from `rustc -vV`, not from a hardcoded table.
const versionInfo = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
const host = /host:\s*(\S+)/.exec(versionInfo)?.[1];
if (!host) {
  console.error("could not read the host triple from `rustc -vV`");
  process.exit(1);
}

execFileSync("cargo", ["build", "--release", "-p", "s08-sidecar"], { stdio: "inherit" });

const ext = process.platform === "win32" ? ".exe" : "";
const src = path.join(root, "target", "release", `s08-sidecar${ext}`);
const dstDir = path.join(root, "src-tauri", "binaries");
const dst = path.join(dstDir, `s08-sidecar-${host}${ext}`);

if (!existsSync(src)) {
  console.error(`expected ${src} after the build, but it is missing`);
  process.exit(1);
}
mkdirSync(dstDir, { recursive: true });
copyFileSync(src, dst);
console.log(`prepared ${dst}`);
