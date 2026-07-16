#!/usr/bin/env node
// Unified npm entrypoint. Mirrors Codex's optional platform-package model,
// while retaining a local workspace fallback for release-package development.

import { spawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = realpathSync(path.join(path.dirname(fileURLToPath(import.meta.url)), ".."));
const require = createRequire(import.meta.url);
const platformPackages = {
  "darwin-arm64": { target: "aarch64-apple-darwin", packageName: "@autoreport/cli-darwin-arm64", directory: "darwin-arm64" },
  "darwin-x64": { target: "x86_64-apple-darwin", packageName: "@autoreport/cli-darwin-x64", directory: "darwin-x64" },
  "linux-arm64": { target: "aarch64-unknown-linux-musl", packageName: "@autoreport/cli-linux-arm64", directory: "linux-arm64" },
  "linux-x64": { target: "x86_64-unknown-linux-musl", packageName: "@autoreport/cli-linux-x64", directory: "linux-x64" },
  "win32-arm64": { target: "aarch64-pc-windows-msvc", packageName: "@autoreport/cli-win32-arm64", directory: "win32-arm64" },
  "win32-x64": { target: "x86_64-pc-windows-msvc", packageName: "@autoreport/cli-win32-x64", directory: "win32-x64" }
};

const platformPackage = platformPackages[`${process.platform}-${process.arch}`];
if (!platformPackage) {
  throw new Error(`Unsupported platform: ${process.platform} (${process.arch})`);
}

function platformPackageRoot() {
  try {
    return path.dirname(require.resolve(`${platformPackage.packageName}/package.json`));
  } catch {
    // `build_npm_package.py --staging-dir` uses this local layout for smoke tests.
    return path.join(packageRoot, "platform-packages", platformPackage.directory);
  }
}

const executable = path.join(
  platformPackageRoot(), "vendor", platformPackage.target, "bin",
  process.platform === "win32" ? "autoreport.exe" : "autoreport",
);
if (!existsSync(executable)) {
  const manager = process.env.npm_config_user_agent?.includes("pnpm/") ? "pnpm add -g" : "npm install -g";
  throw new Error(`Missing optional dependency ${platformPackage.packageName}. Reinstall with: ${manager} @autoreport/cli@latest`);
}

function isPnpmOwnedAutoReportInstall(nodeModulesDir) {
  if (!existsSync(path.join(nodeModulesDir, ".modules.yaml"))) return false;
  try {
    return realpathSync(path.join(nodeModulesDir, "@autoreport", "cli")) === packageRoot;
  } catch {
    return false;
  }
}

function packageManager() {
  const entrypointDir = path.dirname(path.resolve(process.argv[1]));
  for (const startDir of new Set([packageRoot, entrypointDir])) {
    const filesystemRoot = path.parse(startDir).root;
    for (
      let currentDir = startDir;
      currentDir !== filesystemRoot;
      currentDir = path.dirname(currentDir)
    ) {
      if (isPnpmOwnedAutoReportInstall(path.join(currentDir, "node_modules"))) return "pnpm";
    }
    if (isPnpmOwnedAutoReportInstall(path.join(filesystemRoot, "node_modules"))) return "pnpm";
  }

  const userAgent = process.env.npm_config_user_agent ?? "";
  if (/\bbun\//.test(userAgent)) return "bun";
  if (process.env.npm_execpath?.includes("bun")) return "bun";
  if (
    packageRoot.includes(".bun/install/global") ||
    packageRoot.includes(".bun\\install\\global")
  ) return "bun";
  return "npm";
}

const env = { ...process.env, AUTOREPORT_MANAGED_PACKAGE_ROOT: packageRoot };
delete env.AUTOREPORT_MANAGED_BY_NPM;
delete env.AUTOREPORT_MANAGED_BY_PNPM;
delete env.AUTOREPORT_MANAGED_BY_BUN;
const managedBy = packageManager();
env[
  managedBy === "bun"
    ? "AUTOREPORT_MANAGED_BY_BUN"
    : managedBy === "pnpm"
      ? "AUTOREPORT_MANAGED_BY_PNPM"
      : "AUTOREPORT_MANAGED_BY_NPM"
] = "1";
const child = spawn(executable, process.argv.slice(2), { stdio: "inherit", env });
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
