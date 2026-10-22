use std::path::{Path, PathBuf};

/// Build-time secrets Geode can compile in, read via `option_env!`.
const BAKED: &[&str] = &[
    "GEODE_CLIENT_ID",
    "GEODE_CURSEFORGE_KEY",
    "GEODE_GOOGLE_CLIENT_ID",
];

fn main() {
    // `option_env!` is resolved at compile time and Cargo does not otherwise
    // know these inputs exist — without this, setting a key and rebuilding
    // would reuse the cached binary and appear to do nothing.
    for key in BAKED {
        println!("cargo:rerun-if-env-changed={key}");
    }

    for env_file in env_files() {
        println!("cargo:rerun-if-changed={}", env_file.display());
        apply_env_file(&env_file);
    }

    tauri_build::build()
}

/// `.env` is looked for beside this crate and at the repository root, so it
/// works whether you run cargo from `src-tauri/` or npm from the project root.
fn env_files() -> Vec<PathBuf> {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let mut files = vec![crate_dir.join(".env")];
    if let Some(project_root) = crate_dir.parent() {
        files.push(project_root.join(".env"));
    }
    files
}

/// Read `KEY=value` lines and forward the ones we bake in.
///
/// A real environment variable always wins, so a one-off
/// `GEODE_CURSEFORGE_KEY=… npm run tauri build` still overrides the file.
fn apply_env_file(path: &Path) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        if !BAKED.contains(&key) {
            continue;
        }
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            continue; // the ambient environment takes precedence
        }

        let value = value.trim().trim_matches(['"', '\'']);
        if value.is_empty() {
            continue;
        }

        // Cargo captures this line; the value is not echoed to a normal build.
        println!("cargo:rustc-env={key}={value}");
    }
}
