#!/usr/bin/env python3
"""Stage AutoReport's npm meta package and Codex-style platform packages.

The meta package contains only the Node launcher. Each native binary is
published independently as an optional dependency, so npm installs only the
matching target package. This is adapted from Codex's npm staging workflow;
AutoReport has no SDK, responses-proxy, V8, or auxiliary native resources.
"""

from __future__ import annotations

import argparse
import json
import platform
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE_PACKAGE = ROOT / "autoreport-cli"
DIST = ROOT / "dist" / "npm"


@dataclass(frozen=True)
class PlatformPackage:
    slug: str
    npm_name: str
    target: str
    os: str
    cpu: str


PLATFORMS = (
    PlatformPackage("darwin-arm64", "@autoreport/cli-darwin-arm64", "aarch64-apple-darwin", "darwin", "arm64"),
    PlatformPackage("darwin-x64", "@autoreport/cli-darwin-x64", "x86_64-apple-darwin", "darwin", "x64"),
    PlatformPackage("linux-arm64", "@autoreport/cli-linux-arm64", "aarch64-unknown-linux-musl", "linux", "arm64"),
    PlatformPackage("linux-x64", "@autoreport/cli-linux-x64", "x86_64-unknown-linux-musl", "linux", "x64"),
    PlatformPackage("win32-arm64", "@autoreport/cli-win32-arm64", "aarch64-pc-windows-msvc", "win32", "arm64"),
    PlatformPackage("win32-x64", "@autoreport/cli-win32-x64", "x86_64-pc-windows-msvc", "win32", "x64"),
)
BY_SLUG = {item.slug: item for item in PLATFORMS}
HOST_TARGETS = {
    ("darwin", "arm64"): "darwin-arm64", ("darwin", "x86_64"): "darwin-x64",
    ("linux", "aarch64"): "linux-arm64", ("linux", "x86_64"): "linux-x64",
    ("win32", "arm64"): "win32-arm64", ("win32", "amd64"): "win32-x64",
}


def package_version() -> str:
    manifest = json.loads((SOURCE_PACKAGE / "package.json").read_text())
    return manifest["version"]


def host_platform() -> PlatformPackage:
    key = (platform.system().lower(), platform.machine().lower())
    slug = HOST_TARGETS.get(key)
    if slug is None:
        raise SystemExit(f"unsupported host: {platform.system()} {platform.machine()}")
    return BY_SLUG[slug]


def prepare(path: Path, force: bool) -> None:
    if path.exists():
        if not force and any(path.iterdir()):
            raise SystemExit(f"staging directory is not empty: {path} (pass --force to replace it)")
        if force:
            shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


def native_binaries(item: PlatformPackage, vendor_src: Path | None) -> tuple[Path, Path | None]:
    name = "autoreport.exe" if item.os == "win32" else "autoreport"
    helper_name = "autoreport-linux-sandbox"
    if vendor_src:
        candidate = vendor_src / item.target / "bin" / name
        if candidate.is_file():
            helper = vendor_src / item.target / "bin" / helper_name
            if item.os == "linux" and not helper.is_file():
                raise SystemExit(f"Linux sandbox helper not found: {helper}")
            return candidate, helper if item.os == "linux" else None
        raise SystemExit(f"prebuilt binary not found: {candidate}")
    packages = ["-p", "autoreport-cli"]
    if item.os == "linux":
        packages.extend(["-p", "autoreport-linux-sandbox"])
    subprocess.run(["cargo", "build", "--release", *packages, "--target", item.target], cwd=ROOT, check=True)
    candidate = ROOT / "target" / item.target / "release" / name
    if not candidate.is_file():
        raise SystemExit(f"cargo did not produce {candidate}")
    helper = ROOT / "target" / item.target / "release" / helper_name
    if item.os == "linux" and not helper.is_file():
        raise SystemExit(f"cargo did not produce {helper}")
    return candidate, helper if item.os == "linux" else None


def stage_main(destination: Path, version: str) -> None:
    shutil.copytree(SOURCE_PACKAGE / "bin", destination / "bin")
    manifest = json.loads((SOURCE_PACKAGE / "package.json").read_text())
    manifest["version"] = version
    manifest["optionalDependencies"] = {item.npm_name: version for item in PLATFORMS}
    (destination / "package.json").write_text(json.dumps(manifest, indent=2) + "\n")


def stage_platform(destination: Path, item: PlatformPackage, version: str, vendor_src: Path | None) -> None:
    binary, linux_sandbox_helper = native_binaries(item, vendor_src)
    target_dir = destination / "vendor" / item.target / "bin"
    target_dir.mkdir(parents=True)
    output = target_dir / binary.name
    shutil.copy2(binary, output)
    output.chmod(output.stat().st_mode | 0o111)
    if linux_sandbox_helper:
        helper_output = target_dir / linux_sandbox_helper.name
        shutil.copy2(linux_sandbox_helper, helper_output)
        helper_output.chmod(helper_output.stat().st_mode | 0o111)
    manifest = {"name": item.npm_name, "version": version, "license": "MIT", "os": [item.os], "cpu": [item.cpu], "files": ["vendor"]}
    (destination / "package.json").write_text(json.dumps(manifest, indent=2) + "\n")


def npm_pack(directory: Path, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["npm", "pack", "--pack-destination", str(output.parent)], cwd=directory, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=("autoreport", "host", "all", *BY_SLUG), default="host")
    parser.add_argument("--version", default=package_version())
    parser.add_argument("--staging-dir", type=Path)
    parser.add_argument("--vendor-src", type=Path, help="prebuilt vendor root (<target>/bin/autoreport)")
    parser.add_argument("--pack-output", type=Path, help="directory for npm tarballs")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    selected = PLATFORMS if args.package == "all" else (() if args.package == "autoreport" else (host_platform() if args.package == "host" else BY_SLUG[args.package],))
    root = args.staging_dir.resolve() if args.staging_dir else DIST
    prepare(root, args.force)
    if args.package in ("autoreport", "all"):
        destination = root / "autoreport"
        prepare(destination, True)
        stage_main(destination, args.version)
        if args.pack_output: npm_pack(destination, args.pack_output)
        print(f"staged {destination}")
    for item in selected:
        destination = root / item.slug
        prepare(destination, True)
        stage_platform(destination, item, args.version, args.vendor_src.resolve() if args.vendor_src else None)
        if args.pack_output: npm_pack(destination, args.pack_output)
        print(f"staged {destination}")


if __name__ == "__main__":
    main()
