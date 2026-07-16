# Platform npm packages

`scripts/build_npm_package.py` creates the six platform-native optional npm
packages in this directory (or in an explicit staging directory). They are
published as `@autoreport/cli-<platform>-<arch>` and selected by the
`@autoreport/cli` launcher at install time.

The generated `vendor/` binaries are intentionally ignored by Git.
