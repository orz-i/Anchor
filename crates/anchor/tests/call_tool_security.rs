mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;
use serde_json::json;

const TRAVERSAL_PATCH: &str = r#"*** Begin Patch
*** Update File: ../outside-secret.txt
@@
-TOP_SECRET_DO_NOT_READ
+unsafe
*** End Patch
"#;

#[test]
fn internal_exec_supervisor_rejects_specs_outside_harness_store() {
    let temp = tempfile::tempdir().expect("outside supervisor spec");
    let spec = temp.path().join("spec.json");
    fs::write(&spec, b"{}").expect("write fake spec");
    let output = Command::new(env!("CARGO_BIN_EXE_anchor"))
        .arg("exec-supervisor-run")
        .arg(&spec)
        .output()
        .expect("invoke hidden supervisor");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("outside the configured Harness store"),
        "stderr={stderr}"
    );
}

#[test]
fn read_file_rejects_symlink_escape() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "outside-link.txt"}));
    if fx.root.join("outside-link.txt").exists() {
        assert_security_or_policy_err(&out);
    }
}

#[test]
fn exec_command_rejects_workspace_external_directory_link() {
    let fx = tiny_js_fixture();
    let outside = tempfile::tempdir().expect("outside tempdir");
    fs::write(
        outside.path().join("external.txt"),
        "external workspace data",
    )
    .expect("outside file");
    let link = fx.root.join("external-workspace");
    if !create_directory_link(&link, outside.path()) {
        eprintln!("skip directory link test: platform did not allow link creation");
        return;
    }

    let ctx = ctx_for(&fx.root);
    let environment = invoke(&ctx, "check_exec_environment", json!({}));
    let environment = assert_ok(&environment);
    assert_eq!(environment["workspace_exec_available"], false);
    assert_eq!(environment["workspace_link_guard"]["safe"], false);

    let out = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": "python -c \"print('must not run')\"",
            "workdir": "."
        }),
    );
    let error = assert_err(&out);
    assert!(matches!(
        error["error"]["code"].as_str(),
        Some("WORKSPACE_LINK_ESCAPE" | "WORKSPACE_LINK_UNRESOLVED")
    ));
    assert_eq!(error["error"]["details"]["link_path"], "external-workspace");
}

#[test]
fn remove_path_breaks_the_workspace_link_self_lock_without_touching_target() {
    let fx = tiny_js_fixture();
    let outside = tempfile::tempdir().expect("outside tempdir");
    fs::write(outside.path().join("keep.txt"), "keep\n").expect("outside file");
    let link = fx.root.join("external-workspace");
    if !create_directory_link(&link, outside.path()) {
        eprintln!("skip directory link test: platform did not allow link creation");
        return;
    }
    let ctx = ctx_for(&fx.root);

    let blocked = invoke(&ctx, "exec_command", json!({"cmd": "git --version"}));
    let blocked = assert_err(&blocked);
    assert!(matches!(
        blocked["error"]["code"].as_str(),
        Some("WORKSPACE_LINK_ESCAPE" | "WORKSPACE_LINK_UNRESOLVED")
    ));
    assert_eq!(blocked["error"]["details"]["recovery_tool"], "remove_path");

    let removed = invoke(&ctx, "remove_path", json!({"path": "external-workspace"}));
    let removed = assert_ok(&removed);
    assert_eq!(removed["link_like"], true);
    assert_eq!(removed["target_preserved"], true);
    assert!(fs::symlink_metadata(&link).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("keep.txt")).unwrap(),
        "keep\n"
    );

    let recovered = invoke(&ctx, "exec_command", json!({"cmd": "git --version"}));
    assert_eq!(recovered["command_ok"], true);
}

fn create_directory_link(link: &Path, target: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return true;
        }
        std::process::Command::new("cmd.exe")
            .args(["/d", "/s", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

#[test]
fn apply_patch_rejects_traversal_target() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "apply_patch", json!({"patch": TRAVERSAL_PATCH}));
    assert_security_or_policy_err(&out);
}

#[test]
fn read_file_rejects_explicit_external_path_by_default() {
    let fx = malicious_fixture();
    let out = invoke(
        &ctx_for(&fx.root),
        "read_file",
        json!({"path": fx.outside_secret.to_string_lossy()}),
    );
    assert_security_or_policy_err(&out);
}

#[test]
fn external_read_tools_reject_directory_listing_and_search_by_default() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let parent = fx.outside_secret.parent().expect("外部目录");
    let parent_text = parent.to_string_lossy().to_string();

    let listed_result = invoke(&ctx, "list_dir", json!({"path": parent_text}));
    assert_security_or_policy_err(&listed_result);

    let files_result = invoke(
        &ctx,
        "list_files",
        json!({"path": parent.to_string_lossy(), "patterns": ["**/*"]}),
    );
    assert_security_or_policy_err(&files_result);

    let matches_result = invoke(
        &ctx,
        "grep",
        json!({"path": parent.to_string_lossy(), "query": "TOP_SECRET"}),
    );
    assert_security_or_policy_err(&matches_result);
}

#[test]
fn view_image_rejects_explicit_external_path_by_default() {
    let fx = malicious_fixture();
    let image_path = fx
        .outside_secret
        .parent()
        .expect("外部目录")
        .join("outside-probe.png");
    let png_1x1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    fs::write(&image_path, png_1x1).expect("写入测试图片");
    let result = invoke(
        &ctx_for(&fx.root),
        "view_image",
        json!({"path": image_path.to_string_lossy(), "output": "data_url"}),
    );
    assert_security_or_policy_err(&result);
}

#[test]
fn exec_command_rejects_workdir_escape_via_policy() {
    assert_policy_rejects("exec_command", json!({"cmd": "pwd", "workdir": ".."}));
}

#[test]
fn exec_command_allows_workspace_child_process_during_transition() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "python --version", "include_diagnostics": true}),
    );
    let result = assert_ok(&out);
    assert_eq!(result["filesystem_scope"], "workspace");
    assert_eq!(result["sandbox_enforced"], false);
    assert_eq!(result["child_process"], true);
}

#[test]
fn exec_command_rejects_host_scope() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": "python --version",
            "filesystem_scope": "host"
        }),
    );
    assert_eq!(out["error"]["code"], "INVALID_TOOL_ARGUMENTS");
    assert!(out["summary"]
        .as_str()
        .unwrap_or("")
        .contains("filesystem_scope"));
    assert_eq!(
        out["error"]["details"]["reason"],
        "schema_validation_failed"
    );
}

#[test]
fn exec_command_rejects_disallowed_destructive_command() {
    assert_policy_rejects("exec_command", json!({"cmd": "rm -rf /"}));
}

#[test]
fn dangerous_command_requires_operator_dangerous_mode() {
    let trusted = anchor_lib::tools::policy::PolicySettings::default();
    let rejected = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "git reset --hard HEAD", "confirm": true}),
        &trusted,
    );
    assert!(rejected.is_err());

    let dangerous = anchor_lib::tools::policy::PolicySettings {
        permission_mode: "dangerous".into(),
        ..Default::default()
    };
    assert!(anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "git reset --hard HEAD"}),
        &dangerous,
    )
    .is_ok());
}

#[test]
fn deleting_readme_requires_operator_dangerous_mode() {
    let fx = tiny_js_fixture();
    fs::write(fx.root.join("README.md"), "project\n").expect("创建 README");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/README.md\n+++ /dev/null\n@@\n-project\n"
        }),
    );
    assert_eq!(
        out["error"]["code"],
        "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE"
    );

    let mut dangerous_ctx = ctx_for(&fx.root);
    dangerous_ctx.permission_mode = "dangerous".into();
    dangerous_ctx.policy.permission_mode = "dangerous".into();
    let allowed = invoke(
        &dangerous_ctx,
        "apply_patch",
        json!({
            "patch": "--- a/README.md\n+++ /dev/null\n@@\n-project\n"
        }),
    );
    assert_ok(&allowed);
    assert!(!fx.root.join("README.md").exists());
}

#[test]
fn deleting_git_assets_is_always_rejected() {
    let fx = tiny_js_fixture();
    let git_dir = fx.root.join(".git");
    fs::create_dir_all(&git_dir).expect("创建 git 目录");
    fs::write(git_dir.join("config"), "[core]\n").expect("创建 git 配置");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/.git/config\n+++ /dev/null\n@@\n-[core]\n"
        }),
    );
    assert_eq!(out["error"]["code"], "PROTECTED_REPOSITORY_ASSET");
}

#[test]
fn patch_keeps_git_immutable_and_allows_audited_github_writes_in_trusted_mode() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let git = invoke(
        &ctx,
        "apply_patch",
        json!({
            "dry_run": true,
            "patch": "*** Begin Patch\n*** Add File: .git/probe.txt\n+probe\n*** End Patch\n"
        }),
    );
    assert_eq!(git["error"]["code"], "PROTECTED_REPOSITORY_ASSET");

    let github_allowed = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "*** Begin Patch\n*** Add File: .github/workflows/probe.yml\n+name: probe\n*** End Patch\n"
        }),
    );
    assert_ok(&github_allowed);
    assert_eq!(
        fs::read_to_string(fx.root.join(".github/workflows/probe.yml")).expect("read workflow"),
        "name: probe\n"
    );

    let delete_github = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "*** Begin Patch\n*** Delete File: .github/workflows/probe.yml\n*** End Patch\n"
        }),
    );
    assert_eq!(
        delete_github["error"]["code"],
        "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE"
    );

    let git_still_blocked = invoke(
        &ctx,
        "apply_patch",
        json!({
            "dry_run": true,
            "patch": "*** Begin Patch\n*** Add File: .git/dangerous-probe.txt\n+probe\n*** End Patch\n"
        }),
    );
    assert_eq!(
        git_still_blocked["error"]["code"],
        "PROTECTED_REPOSITORY_ASSET"
    );

    let generic_remove = invoke(
        &ctx,
        "remove_path",
        json!({"path": ".github/workflows/probe.yml"}),
    );
    assert_eq!(generic_remove["error"]["code"], "PROTECTED_PATH");
    assert!(fx.root.join(".github/workflows/probe.yml").is_file());

    let mut dangerous_ctx = ctx_for(&fx.root);
    dangerous_ctx.permission_mode = "dangerous".into();
    dangerous_ctx.policy.permission_mode = "dangerous".into();
    let dangerous_generic_remove = invoke(
        &dangerous_ctx,
        "remove_path",
        json!({"path": ".github/workflows/probe.yml"}),
    );
    assert_eq!(dangerous_generic_remove["error"]["code"], "PROTECTED_PATH");

    let dangerous_patch_delete = invoke(
        &dangerous_ctx,
        "apply_patch",
        json!({
            "patch": "*** Begin Patch\n*** Delete File: .github/workflows/probe.yml\n*** End Patch\n"
        }),
    );
    assert_ok(&dangerous_patch_delete);
    assert!(!fx.root.join(".github/workflows/probe.yml").exists());
}

#[test]
fn destructive_command_targeting_git_is_always_rejected() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    let error = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "rm -rf .git"}),
        &policy,
    )
    .expect_err("删除 .git 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_delete_git_assets() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    let error = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"import shutil; shutil.rmtree('.git')\""
        }),
        &policy,
    )
    .expect_err("解释器删除 .git 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_delete_github_assets() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    let error = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"import os; os.remove('.github/workflows/ci.yml')\""
        }),
        &policy,
    )
    .expect_err("解释器删除 .github 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_write_outside_workspace_scope() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    let error = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"from pathlib import Path; Path('../outside.txt').write_text('x')\"",
            "filesystem_scope": "workspace"
        }),
        &policy,
    )
    .expect_err("workspace scope 不得写入外部路径");
    assert!(error.0.contains("WORKSPACE_PATH_PROTECTED"));
}

#[test]
fn interpreter_command_cannot_write_git_files() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    let error = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"from pathlib import Path; Path('.git/config').write_text('x')\"",
            "filesystem_scope": "workspace"
        }),
        &policy,
    )
    .expect_err("普通解释器不得写入 .git");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn apply_patch_allows_modifying_a_normal_file() {
    let fx = tiny_js_fixture();
    let target = fx.root.join("src/normal.txt");
    fs::write(&target, "before\n").expect("创建待修改文件");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/src/normal.txt\n+++ b/src/normal.txt\n@@\n-before\n+after\n"
        }),
    );
    assert_ok(&out);
    assert!(fs::read_to_string(target)
        .expect("读取修改后的文件")
        .contains("after"));
}

#[test]
fn apply_patch_allows_deleting_a_normal_file() {
    let fx = tiny_js_fixture();
    let target = fx.root.join("src/delete-me.js");
    fs::write(&target, "delete me\n").expect("创建待删除文件");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/src/delete-me.js\n+++ /dev/null\n@@\n-delete me\n"
        }),
    );
    assert_ok(&out);
    assert!(!target.exists());
}

#[test]
fn apply_patch_rejects_absolute_path_target() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/C:/outside-secret.txt\n+++ b/C:/outside-secret.txt\n@@\n-TOP_SECRET_DO_NOT_READ\n+unsafe\n"
        }),
    );
    assert_security_or_policy_err(&out);
}

#[test]
fn exec_command_allows_python_c_but_rejects_shell_escape() {
    let policy = anchor_lib::tools::policy::PolicySettings::default();
    assert!(anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "python -c \"import os; print(os.getcwd())\""}),
        &policy,
    )
    .is_ok());
    assert_policy_rejects(
        "exec_command",
        json!({"cmd": "python -c \"print(1)\" && rm -rf /"}),
    );
}

#[test]
fn exec_command_rejects_shell_chaining() {
    assert_policy_rejects("exec_command", json!({"cmd": "echo hi && rm -rf /"}));
}

#[test]
fn safe_permission_mode_blocks_network_looking_command() {
    let policy = anchor_lib::tools::policy::PolicySettings {
        permission_mode: "safe".into(),
        ..Default::default()
    };
    let err = anchor_lib::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "curl https://example.com"}),
        &policy,
    )
    .expect_err("network command should be blocked in safe mode");
    assert!(err.0.contains("Network-looking"));
}
