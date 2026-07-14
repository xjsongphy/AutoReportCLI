#!/usr/bin/env python3
"""Build and stage the native CLI exactly where the npm launcher expects it.

Usage: python3 autoreport-cli/scripts/build_npm_package.py [--target <rust-target>]
"""

from __future__ import annotations

import argparse
import platform
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "autoreport-cli"
TARGETS = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("Linux", "x86_64"): "x86_64-unknown-linux-musl",
    ("Windows", "ARM64"): "aarch64-pc-windows-msvc",
    ("Windows", "AMD64"): "x86_64-pc-windows-msvc",
}


def host_target() -> str:
    target = TARGETS.get((platform.system(), platform.machine()))
    if target is None:
        raise SystemExit(f"unsupported host: {platform.system()} {platform.machine()}")
    return target


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", default=host_target())
    args = parser.parse_args()
    subprocess.run(
        ["cargo", "build", "--release", "-p", "autoreport-cli", "--target", args.target],
        cwd=ROOT,
        check=True,
    )
    binary = ROOT / "target" / args.target / "release" / ("autoreport.exe" if args.target.endswith("windows-msvc") else "autoreport")
    if not binary.is_file():
        raise SystemExit(f"cargo did not produce {binary}")
    destination = PACKAGE / "vendor" / args.target / "bin" / binary.name
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, destination)
    destination.chmod(destination.stat().st_mode | 0o111)
    print(f"staged {destination.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
