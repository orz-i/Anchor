use std::path::Path;
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

fn main() {
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"),
    );
    let repository = manifest_dir.parent().unwrap_or(&manifest_dir);
    let git_sha = std::env::var("ANCHOR_BUILD_GIT_SHA")
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
    println!("cargo:rerun-if-env-changed=ANCHOR_BUILD_GIT_SHA");
    if let Some(git_dir) = git_value(repository, &["rev-parse", "--git-dir"]) {
        let git_dir = std::path::PathBuf::from(git_dir);
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            repository.join(git_dir)
        };
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    }
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
