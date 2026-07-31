// Cargo-only build of the vendored `cargo-bin` helper.
//
// The `runfiles` crate's `rlocation!` macro reads `REPOSITORY_NAME` via `env!`
// at compile time (Bazel normally injects it). We never execute the runfiles
// path under Cargo — `find_resource!` only consults runfiles when
// `RUNFILES_MANIFEST_ONLY` is set, which Cargo never does — but the macro still
// has to *expand*. Provide a placeholder so it compiles; the value is unused at
// runtime.
fn main() {
    println!("cargo:rustc-env=REPOSITORY_NAME=autoreport");
}
