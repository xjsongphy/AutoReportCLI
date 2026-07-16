fn main() {
    println!("cargo:rerun-if-env-changed=AUTOREPORT_BWRAP_SHA256");
}
