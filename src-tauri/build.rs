use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_value(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_path(root: &Path, path: &str) -> Option<PathBuf> {
    let value = git_value(root, &["rev-parse", "--git-path", path])?;
    let resolved = PathBuf::from(value);
    Some(if resolved.is_absolute() {
        resolved
    } else {
        root.join(resolved)
    })
}

fn watch_git_path(root: &Path, path: &str) {
    if let Some(path) = git_path(root, path) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn admin_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn collect_admin_assets(root: &Path, current: &Path, assets: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_admin_assets(root, &path, assets);
        } else if file_type.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let key = relative.to_string_lossy().replace('\\', "/");
            assets.push((key, path));
        }
    }
}

fn generate_admin_assets(repository: &Path) {
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let build_dir = repository.join("build");
    println!("cargo:rerun-if-changed={}", build_dir.display());

    let mut assets = Vec::new();
    if build_dir.is_dir() {
        collect_admin_assets(&build_dir, &build_dir, &mut assets);
        assets.sort_by(|left, right| left.0.cmp(&right.0));
    }

    let mut generated = String::new();
    generated.push_str(&format!(
        "const ADMIN_UI_EMBEDDED: bool = {};\n",
        !assets.is_empty()
    ));
    generated.push_str(
        "fn embedded_admin_asset(path: &str) -> Option<AdminStaticAsset> {\n    match path {\n",
    );
    for (key, path) in assets {
        let source = format!("{:?}", path.to_string_lossy().as_ref());
        let content_type = admin_content_type(&path);
        generated.push_str(&format!(
            "        {key:?} => Some(AdminStaticAsset {{ content_type: {content_type:?}, body: include_bytes!({source}) }}),\n"
        ));
    }
    generated.push_str("        _ => None,\n    }\n}\n");
    fs::write(out_dir.join("admin_assets.rs"), generated)
        .expect("failed to generate embedded Web Admin assets");
}

fn main() {
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"),
    );
    let repository = manifest_dir.parent().unwrap_or(&manifest_dir);
    // Do not use ANCHOR_BUILD_GIT_SHA itself as the input override. Cargo
    // propagates values emitted with `cargo:rustc-env` into `cargo run` child
    // processes, so a daemon started through `cargo run` can legitimately
    // carry an old ANCHOR_BUILD_GIT_SHA in its runtime environment. Reusing
    // that value as a future build input would permanently pin subsequent
    // builds to the daemon's old revision.
    let git_sha = std::env::var("ANCHOR_BUILD_GIT_SHA_OVERRIDE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git_value(repository, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let git_dirty = git_value(
        repository,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .is_some_and(|value| !value.is_empty());
    println!("cargo:rustc-env=ANCHOR_BUILD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=ANCHOR_BUILD_GIT_DIRTY={git_dirty}");
    println!(
        "cargo:rustc-env=ANCHOR_BUILD_WORKSPACE={}",
        repository.display()
    );
    println!("cargo:rerun-if-env-changed=ANCHOR_BUILD_GIT_SHA_OVERRIDE");
    watch_git_path(repository, "HEAD");
    watch_git_path(repository, "index");
    if let Some(head_ref) = git_value(repository, &["symbolic-ref", "-q", "HEAD"]) {
        // Watching only .git/HEAD is insufficient for normal branches because
        // HEAD usually contains a stable `ref: refs/heads/...` pointer while
        // the referenced branch file changes on each commit. `--git-path`
        // also resolves the common Git directory correctly for linked
        // worktrees, so this keeps embedded build identity fresh in both
        // checkout modes.
        watch_git_path(repository, &head_ref);
    }
    watch_git_path(repository, "packed-refs");
    generate_admin_assets(repository);
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
