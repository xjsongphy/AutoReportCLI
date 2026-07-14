#!/usr/bin/env node
// npm launcher for the platform-specific AutoReport native binary.

import { spawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = realpathSync(path.join(path.dirname(fileURLToPath(import.meta.url)), ".."));
const targetByPlatform = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "linux-x64": "x86_64-unknown-linux-musl",
  "win32-arm64": "aarch64-pc-windows-msvc",
  "win32-x64": "x86_64-pc-windows-msvc"
};

const target = targetByPlatform[`${process.platform}-${process.arch}`];
if (!target) {
  throw new Error(`Unsupported platform: ${process.platform} (${process.arch})`);
}

const executable = path.join(packageRoot, "vendor", target, "bin", process.platform === "win32" ? "autoreport.exe" : "autoreport");
if (!existsSync(executable)) {
  throw new Error(`Missing native binary for ${target}. Reinstall @autoreport/cli for this platform.`);
}

const child = spawn(executable, process.argv.slice(2), { stdio: "inherit", env: process.env });
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => child.kill(signal));
}
child.on("error", (error) => {
  console.error(error);
  process.exitCode = 1;
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
