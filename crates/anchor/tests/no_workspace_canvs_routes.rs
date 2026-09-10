use std::fs;
use std::path::{Path, PathBuf};

fn source(path: &str) -> String {
    fs::read_to_string(crate_path(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn crate_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

#[test]
fn workspace_mcp_listener_does_not_publish_canvs_routes() {
    let listener = source("src/mcp/listener.rs");
    for retired in [
        "/canvs",
        "/canvas",
        "canvs_task_list_page",
        "canvs_task_detail_page",
    ] {
        assert!(
            !listener.contains(retired),
            "Workspace MCP listener must not publish retired task page surface: {retired}"
        );
    }

    let lib = source("src/lib.rs");
    assert!(!lib.contains("mod canvs_web;"));
    assert!(!Path::new(&crate_path("src/canvs_web.rs")).exists());
}

#[test]
fn admin_surface_uses_top_level_task_commands_only() {
    let admin = source("src/admin.rs");
    assert!(admin.contains("\"list_tasks\""));
    assert!(admin.contains("\"get_task_snapshot\""));
    for retired in [
        "get_canvs_snapshot",
        "list_canvs_tasks",
        "get_canvs_task_snapshot",
    ] {
        assert!(
            !admin.contains(retired),
            "retired workspace-scoped Admin command must not return: {retired}"
        );
    }

    let management = source("src/management.rs");
    assert!(management.contains("pub(crate) fn list_tasks()"));
    assert!(management.contains("pub(crate) fn get_task_snapshot("));
}
