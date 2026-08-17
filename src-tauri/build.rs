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
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
