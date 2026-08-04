# GitHub Actions Pipeline Optimization Design

Date: 2026-08-03

## Goal

Reduce duplicated GitHub Actions runs and repeated Rust compilation while keeping
platform-specific correctness checks and full native npm payload validation.
The refactor targets the four existing workflows without changing application
code, package contents, or release build settings.

## Current problems

- `ci.yml`, `linux-sandbox.yml`, `macos-sandbox.yml`, and
  `windows-sandbox.yml` all react to both `push` and `pull_request`, so a
  branch update associated with a pull request can start duplicate runs.
- Linux and Windows sandbox workflows perform release builds and npm staging
  for every pull request. The macOS staging script also invokes a release
  build when no prebuilt vendor directory is supplied.
- Rust cache setup is duplicated and does not distinguish native checks from
  the Linux musl release target.
- The current CI matrix runs the broad workspace lint/test workload on macOS,
  even though macOS-specific sandbox tests are already separate.

## Chosen architecture

Replace the four workflows with one `.github/workflows/ci.yml`:

1. A `rustfmt` job runs on pull requests and pushes to `main`.
2. A `platform-check` matrix runs one job each on Ubuntu, macOS, and Windows.
   Each matrix entry keeps the platform-specific setup and tests:
   - Ubuntu runs Clippy, the workspace tests excluding native sandbox crates,
     and native Linux sandbox tests.
   - macOS runs workspace `cargo check` and the Seatbelt/tools isolation tests.
   - Windows builds test helpers, checks the CLI, and runs native sandbox tests.
3. A `package` matrix runs only for `main` pushes, `v*` tags, and manual
   dispatch. It performs the existing full release build and npm payload
   validation for Linux musl, Windows MSVC, and the host macOS target.
4. A local composite action at `.github/actions/setup-rust/action.yml`
   centralizes stable toolchain setup, optional components/targets, and
   platform-specific Cargo caching.

The workflow keeps `pull_request`, `push` for `main` and `v*` tags, and
`workflow_dispatch`. Tags skip ordinary checks and run packaging only; manual
dispatch runs the complete pipeline. All jobs use `permissions: contents: read`
and a workflow/ref concurrency group with cancellation enabled.

## Caching strategy

Each matrix entry receives a distinct shared cache key:

- `ci-linux`, `ci-macos`, and `ci-windows` for native checks;
- `package-linux-musl`, `package-macos-release`, and
  `package-windows-release` for release packaging.

Caches use `cache-on-failure: true`. The musl target is installed only in the
Linux packaging entry, so native CI jobs do not pay for cross-compilation setup.

## Compatibility and non-goals

- The release profile remains unchanged (`opt-level = 3`, thin LTO).
- No path filters are added, avoiding skipped required checks for documentation
  or workflow-only changes.
- No npm publish or GitHub Release upload is introduced; packaging remains
  staging and payload verification, as in the existing workflows.
- Existing uncommitted application, artifact, and image changes are outside
  this design and must not be staged or modified.
- The refactor intentionally changes check names from the old per-workflow
  names to `rustfmt`, `platform-check (platform)`, and `package (platform)`.
  Repository branch protection should point required checks at the new names
  after rollout.

## Verification

Before handoff, validate that:

- only the unified workflow and composite action replace the old workflow files;
- PR events do not contain release build or package steps;
- main/tag/manual events reach the release matrix;
- all referenced package paths, Cargo package names, targets, and helper
  environment variables match the repository scripts;
- YAML parses and `actionlint` passes when available;
- the final diff contains no unrelated worktree changes.
