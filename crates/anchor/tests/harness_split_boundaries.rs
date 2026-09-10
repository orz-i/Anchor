use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            rust_files(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}

#[test]
fn tool_context_exposes_separate_task_and_coding_harness_authorities() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let context = fs::read_to_string(root.join("src/tools/context.rs")).expect("context source");

    assert!(context.contains("pub task_harness: TaskHarness"));
    assert!(context.contains("pub coding_harness: CodingHarness"));
    assert!(!context.contains("pub harness:"));
}

#[test]
fn product_code_cannot_reintroduce_the_monolithic_harness_authority() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_root = root.join("src");
    let mut files = Vec::new();
    rust_files(&source_root, &mut files);

    for path in files {
        let relative = path
            .strip_prefix(&source_root)
            .expect("source-relative path")
            .to_string_lossy()
            .replace('\\', "/");
        if matches!(relative.as_str(), "harness/state.rs" | "harness/split.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read rust source");
        let compact = source
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();

        assert!(
            !source.contains("crate::harness::Harness::")
                && !source.contains("crate::harness::Harness,")
                && !source.contains("crate::harness::Harness;")
                && !source.contains("crate::harness::{Harness,"),
            "monolithic Harness type leaked into {relative}"
        );
        assert!(
            !source.contains("Harness::new("),
            "monolithic Harness constructor leaked into {relative}"
        );
        assert!(
            !compact.contains(".harness."),
            "monolithic ToolContext Harness field leaked into {relative}"
        );
    }
}

#[test]
fn public_harness_module_exports_only_split_authorities() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let module = fs::read_to_string(root.join("src/harness/mod.rs")).expect("harness module");

    assert!(module.contains("CodingHarness"));
    assert!(module.contains("TaskHarness"));
    assert!(module.contains("split_harness"));
    assert!(!module.contains("pub use state::Harness"));
    assert!(!module.contains("pub(crate) use state::Harness"));
}

#[test]
fn authority_apis_do_not_cross_task_and_coding_domains() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let split = fs::read_to_string(root.join("src/harness/split.rs")).expect("split source");
    let task_start = split.find("impl TaskHarness").expect("TaskHarness impl");
    let coding_start = split
        .find("impl CodingHarness")
        .expect("CodingHarness impl");
    let task_api = &split[task_start..coding_start];
    let coding_api = &split[coding_start..];

    for forbidden in [
        "start_task_in_git_worktree",
        "attach_git_worktree",
        "check_baseline",
        "record_verification",
        "record_recovery",
        "save_change_set",
        "save_close_outbox",
    ] {
        assert!(
            !task_api.contains(forbidden),
            "coding API {forbidden} leaked into TaskHarness"
        );
    }

    for forbidden in [
        "update_steps",
        "revise_plan",
        "configure_task",
        "start_slice",
        "update_slice",
        "complete_slice",
        "bind_session",
        "reclaim_session",
    ] {
        assert!(
            !coding_api.contains(forbidden),
            "task API {forbidden} leaked into CodingHarness"
        );
    }
}
