# npm release staging

This is the AutoReport adaptation of Codex's npm staging model. `@autoreport/cli`
is a lightweight launcher; native binaries are published as the six optional
packages `@autoreport/cli-<platform>-<arch>`.

Build the current host package for local testing:

```bash
python3 autoreport-cli/scripts/build_npm_package.py --package host --force
```

Stage the meta package and all platform packages from CI-provided binaries:

```bash
python3 autoreport-cli/scripts/build_npm_package.py --package all --vendor-src /path/to/vendor --force
```

`--vendor-src` must contain `<rust-target>/bin/autoreport[.exe]`. Cross-target
builds require the appropriate Rust target and linker; use CI artifacts for
release builds rather than relying on local cross compilation.
