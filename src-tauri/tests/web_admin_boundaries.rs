use std::fs;
use std::path::{Path, PathBuf};

fn collect_files(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            collect_files(&path, output);
        } else {
            output.push(path);
        }
    }
}

#[test]
fn frontend_tauri_dependencies_are_confined_to_platform_adapters() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let frontend = manifest.parent().expect("repo root").join("src");
    let mut files = Vec::new();
    collect_files(&frontend, &mut files);

    let mut violations = Vec::new();
    for path in files {
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "svelte")
        ) {
            continue;
        }
        let relative = path
            .strip_prefix(&frontend)
            .expect("frontend relative path");
        let allowed = relative == Path::new("lib/api/invoke.ts")
            || relative.starts_with(Path::new("lib/platform"));
        let source = fs::read_to_string(&path).expect("read frontend source");
        if !allowed && source.contains("@tauri-apps") {
            violations.push(relative.display().to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "Tauri imports escaped the platform adapters: {violations:?}"
    );
}

#[test]
fn desktop_shell_does_not_own_runtime_resources() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_state = fs::read_to_string(manifest.join("src/app_state.rs")).expect("app state");
    let desktop = fs::read_to_string(manifest.join("src/lib.rs")).expect("desktop lib");

    assert!(!app_state.contains("RuntimeSupervisor"));
    assert!(!desktop.contains("spawn_desktop_maintenance"));
    assert!(!desktop.contains("gateway::stop().await"));
    assert!(!desktop.contains("shutdown_all()"));
}
