use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root")
        .to_path_buf()
}

#[test]
fn package_defaults_to_web_admin_and_cli_release() {
    let package = package_json();
    let scripts = package["scripts"].as_object().expect("package scripts");

    assert_eq!(scripts["start"].as_str(), Some("pnpm admin:serve"));
    assert_eq!(scripts["release:build"].as_str(), Some("pnpm cli:build"));
    assert!(scripts["admin:serve"]
        .as_str()
        .expect("admin serve script")
        .contains("--bin anchor -- admin serve"));

    for key in [
        "legacy:desktop",
        "legacy:desktop:build",
        "legacy:desktop:manifest",
        "legacy:tauri",
    ] {
        assert!(
            scripts.contains_key(key),
            "missing explicit legacy target: {key}"
        );
    }
    for key in ["desktop", "desktop:build", "desktop:manifest", "tauri"] {
        let command = scripts[key].as_str().expect("compatibility desktop script");
        assert!(
            command.contains("deprecated-desktop.mjs"),
            "desktop compatibility alias must emit a deprecation warning: {key}"
        );
    }

    for (key, value) in scripts {
        if key.contains("desktop") || key.contains("tauri") || key.starts_with("legacy:") {
            continue;
        }
        let command = value.as_str().expect("script command");
        assert!(
            !command.contains("run-with-rust-toolchain.mjs tauri")
                && !command.contains("@tauri-apps")
                && !command.contains("deprecated-desktop.mjs"),
            "default/non-legacy npm script still depends on Tauri: {key}={command}"
        );
    }
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(repository_root().join(path)).expect("read repository file")
}

fn package_json() -> serde_json::Value {
    serde_json::from_str(&read("package.json")).expect("parse package.json")
}

#[test]
fn cargo_defaults_to_cli_and_keeps_desktop_explicit() {
    let manifest = read("src-tauri/Cargo.toml");
    let build_rs = read("src-tauri/build.rs");

    assert!(manifest.contains("default = [\"cli\"]"));
    assert!(manifest.contains(
        "desktop = [\"dep:tauri\", \"dep:tauri-plugin-dialog\", \"dep:tauri-build\", \"cli\"]"
    ));
    assert!(manifest.contains("tauri-build = { version = \"2\", features = [], optional = true }"));
    assert!(manifest.contains(
        "name = \"anchor-desktop\"\npath = \"src/main.rs\"\nrequired-features = [\"desktop\"]"
    ));
    assert!(manifest.contains(
        "name = \"anchor\"\npath = \"src/bin/anchor.rs\"\nrequired-features = [\"cli\"]"
    ));
    assert!(build_rs.contains("#[cfg(feature = \"desktop\")]"));
    assert!(build_rs.contains("tauri_build::build();"));
    assert!(!build_rs.contains("CARGO_FEATURE_DESKTOP"));
}

#[test]
fn desktop_shell_remains_a_legacy_adapter_not_runtime_owner() {
    let app_state = read("src-tauri/src/app_state.rs");
    let lib = read("src-tauri/src/lib.rs");
    let legacy = read("src-tauri/src/legacy_desktop.rs");

    for forbidden in [
        "RuntimeSupervisor",
        "TunnelSupervisor",
        "GatewayRuntime",
        "gateway_daemon::spawn",
        "daemon::spawn",
    ] {
        assert!(
            !app_state.contains(forbidden),
            "desktop AppState regained runtime ownership: {forbidden}"
        );
    }
    assert!(lib.contains("mod legacy_desktop;"));
    assert!(lib.contains("pub use legacy_desktop::run;"));
    assert!(!lib.contains("tauri::Builder"));
    assert!(!lib.contains("tauri::generate_handler!"));
    assert!(legacy.contains("tauri::generate_handler!"));
    assert!(legacy.contains("AppState::new()"));
}

#[test]
fn frontend_tauri_imports_are_confined_to_platform_adapters() {
    let root = repository_root().join("src");
    let mut violations = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read frontend directory") {
            let entry = entry.expect("frontend entry");
            let path = entry.path();
            if entry.file_type().expect("frontend file type").is_dir() {
                stack.push(path);
                continue;
            }
            if !matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("ts" | "svelte")
            ) {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read frontend source");
            if !source.contains("@tauri-apps") {
                continue;
            }
            let relative = path.strip_prefix(&root).expect("frontend relative path");
            let allowed = relative == Path::new("lib/api/invoke.ts")
                || relative.starts_with(Path::new("lib/platform"));
            if !allowed {
                violations.push(relative.display().to_string());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Tauri imports escaped legacy adapter boundary: {violations:?}"
    );
}
