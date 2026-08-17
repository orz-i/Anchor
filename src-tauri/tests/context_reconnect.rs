use anchor_lib::tools::ToolContext;

#[test]
fn fresh_context_rebinds_unique_session_backed_task_for_host_scope() {
    let workspace = tempfile::tempdir().expect("workspace");
    let harness_root = tempfile::tempdir().expect("harness");
    let first = ToolContext::for_test(
        workspace.path().to_path_buf(),
        harness_root.path().to_path_buf(),
    )
    .expect("first context");
    let task = first
        .harness
        .start_task("persisted reconnect")
        .expect("task");
    first
        .harness
        .bind_session(
            &task.id,
            "ses_0123456789abcdef0123456789abcdef",
            "docs/session/ses_0123456789abcdef0123456789abcdef.md",
        )
        .expect("bind persisted session");

    // New ToolContext models an MCP/gateway reconnect: transport bindings are empty,
    // while the Session-backed Harness Task remains durable on disk.
    let reconnected = ToolContext::for_test(
        workspace.path().to_path_buf(),
        harness_root.path().to_path_buf(),
    )
    .expect("reconnected context");
    reconnected.bind_cursor_scope_for_session(
        "transport-after-reconnect",
        Some("host-session:conversation-reconnect"),
    );
    assert_eq!(
        reconnected
            .task_for_session(Some("transport-after-reconnect"))
            .map(|task| task.id),
        Some(task.id)
    );
}
