use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(repository_root().join(path)).expect("read repository file")
}

fn collect_source_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        if entry.file_type().expect("source file type").is_dir() {
            collect_source_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[test]
fn package_and_lockfiles_have_no_tauri_or_svelte_dependencies_or_scripts() {
    let package: serde_json::Value =
        serde_json::from_str(&read("package.json")).expect("parse package.json");
    let package_text = package.to_string();
    assert_eq!(package["name"].as_str(), Some("anchor"));
    assert_eq!(
        package["scripts"]["start"].as_str(),
        Some("pnpm admin:serve")
    );
    assert_eq!(
        package["scripts"]["release:build"].as_str(),
        Some("pnpm cli:build")
    );
    assert!(!package_text.contains("@tauri-apps"));
    assert!(!package_text.contains("svelte"));
    let scripts = package["scripts"].as_object().expect("package scripts");
    for (name, command) in scripts {
        assert!(!name.contains("tauri") && !name.contains("desktop"));
        let command = command.as_str().expect("script command");
        for forbidden in [
            "@tauri-apps",
            "run-with-rust-toolchain.mjs tauri",
            "deprecated-desktop.mjs",
            "legacy:desktop",
        ] {
            assert!(
                !command.contains(forbidden),
                "npm script {name} retains Tauri runtime entrypoint: {forbidden}"
            );
        }
    }

    let lock = read("pnpm-lock.yaml");
    assert!(
        !lock.contains("@tauri-apps"),
        "Tauri package remains in pnpm-lock.yaml"
    );
    assert!(
        !lock.contains("svelte"),
        "Svelte package remains in pnpm-lock.yaml"
    );
    assert!(!repository_root().join("package-lock.json").exists());
}

#[test]
fn cargo_has_only_the_anchor_cli_product() {
    let manifest = read("crates/anchor/Cargo.toml");
    let lock = read("crates/anchor/Cargo.lock");
    let build_rs = read("crates/anchor/build.rs");

    assert!(manifest.contains("name = \"anchor\""));
    assert!(manifest.contains("default = [\"cli\"]"));
    assert!(manifest.contains("name = \"anchor\"\npath = \"src/bin/anchor.rs\""));
    for forbidden in [
        "feature = \"desktop\"",
        "desktop = [",
        "anchor-desktop",
        "tauri-build",
        "tauri-plugin",
        "tauri =",
        "tauri_build",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "Cargo manifest retains {forbidden}"
        );
        assert!(
            !build_rs.contains(forbidden),
            "build.rs retains {forbidden}"
        );
    }
    assert!(!lock.contains("name = \"anchor-desktop\""));
    assert!(!lock.contains("name = \"tauri\""));
    assert!(!lock.contains("name = \"tauri-plugin"));
}

#[test]
fn tauri_application_files_are_physically_absent() {
    let root = repository_root();
    assert!(
        !root.join("src-tauri").exists(),
        "retired src-tauri directory still exists"
    );
    for path in [
        "src-tauri/tauri.conf.json",
        "src-tauri/src/main.rs",
        "src-tauri/src/legacy_desktop.rs",
        "src-tauri/src/app_state.rs",
        "src-tauri/src/commands",
        "src-tauri/capabilities",
        "src-tauri/icons",
        "dev-desktop.cmd",
        "scripts/deprecated-desktop.mjs",
        "scripts/prepare-desktop-build.mjs",
        "scripts/finalize-desktop-build.mjs",
        "src/lib/platform/runtime.ts",
    ] {
        assert!(
            !root.join(path).exists(),
            "retired Tauri path still exists: {path}"
        );
    }
}

#[test]
fn active_frontend_and_rust_sources_have_no_tauri_runtime_references() {
    let root = repository_root();
    let mut files = Vec::new();
    collect_source_files(&root.join("src"), &mut files);
    collect_source_files(&root.join("crates/anchor/src"), &mut files);
    files.push(root.join("scripts/run-with-rust-toolchain.mjs"));

    let mut violations = Vec::new();
    for path in files {
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read active source");
        for forbidden in [
            "@tauri-apps",
            "tauri::",
            "tauri_build",
            "feature = \"desktop\"",
            "anchor-desktop.exe",
            "DESKTOP_EXECUTABLE_NAME",
            "BUNDLE_ID",
        ] {
            if source.contains(forbidden) {
                violations.push(format!("{}: {forbidden}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "active Tauri references remain: {violations:?}"
    );
}

#[test]
fn compatibility_data_path_is_not_rewritten_by_tauri_removal() {
    let brand = read("crates/anchor/src/brand.rs");
    assert!(brand.contains("APP_CONFIG_DIR_NAME: &str = \"anchor\""));
    assert!(brand.contains("CONFIG_DIR_ENV: &str = \"ANCHOR_CONFIG_DIR\""));
}
