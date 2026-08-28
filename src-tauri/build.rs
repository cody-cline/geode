fn main() {
    // `option_env!` is resolved at compile time, and Cargo does not otherwise
    // know these inputs exist — without this, setting a key and rebuilding
    // would reuse the cached binary and appear to do nothing.
    println!("cargo:rerun-if-env-changed=GEODE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=GEODE_CURSEFORGE_KEY");

    tauri_build::build()
}
