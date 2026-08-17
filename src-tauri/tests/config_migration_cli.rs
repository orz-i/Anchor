use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn anchor() -> &'static str {
    env!("CARGO_BIN_EXE_anchor")
}

fn run(args: &[String]) -> Output {
    Command::new(anchor())
        .args(args)
        .output()
        .expect("run anchor CLI")
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}", output_text(output));
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read json file")).expect("parse json file")
}

fn cli_args(config_dir: &Path, args: &[&str]) -> Vec<String> {
    let mut values = vec![
        "--config-dir".to_string(),
        config_dir.to_string_lossy().into_owned(),
    ];
    values.extend(args.iter().map(|value| (*value).to_string()));
    values
}

fn import_args(
    config_dir: &Path,
    bundle: &Path,
    passphrase_file: &Path,
    workspace_id: &str,
    target_workspace: &Path,
) -> Vec<String> {
    cli_args(
        config_dir,
        &[
            "import",
            &bundle.to_string_lossy(),
            "--passphrase-file",
            &passphrase_file.to_string_lossy(),
            "--workspace-path",
            &format!("{workspace_id}={}", target_workspace.display()),
        ],
    )
}

fn first_profile(root: &Value) -> &Value {
    root.get("profiles")
        .and_then(Value::as_array)
        .and_then(|profiles| profiles.first())
        .expect("first profile")
}

#[test]
fn portable_config_roundtrip_recovers_incompatible_dpapi_and_preserves_registration_identity() {
    let temp = tempfile::tempdir().expect("migration tempdir");
    let source_config = temp.path().join("source-config");
    let target_config = temp.path().join("target-config");
    let source_workspace = temp.path().join("source-workspace");
    let target_workspace = temp.path().join("target-workspace");
    let bundle = temp.path().join("anchor-migration.json");
    let passphrase_file = temp.path().join("migration.pass");
    let wrong_passphrase_file = temp.path().join("wrong.pass");
    fs::create_dir_all(&source_workspace).expect("source workspace");
    fs::create_dir_all(&target_workspace).expect("target workspace");
    fs::write(&passphrase_file, b"portable-migration-passphrase\n").expect("passphrase");
    fs::write(&wrong_passphrase_file, b"definitely-the-wrong-passphrase\n")
        .expect("wrong passphrase");

    let register = run(&cli_args(
        &source_config,
        &[
            "workspace",
            "register",
            &source_workspace.to_string_lossy(),
            "--name",
            "migration-e2e",
        ],
    ));
    assert_success(&register);
    let workspace_id = String::from_utf8_lossy(&register.stdout)
        .split('\t')
        .nth(1)
        .expect("registered workspace id")
        .to_string();

    let source_profiles_path = source_config.join("data/profiles.json");
    let source_profiles = json_file(&source_profiles_path);
    let source_profile = first_profile(&source_profiles);
    let source_mcp_client_id = source_profile["auth"]["oauth_client_id"]
        .as_str()
        .expect("MCP client id")
        .to_string();
    let source_actions_client_id = source_profile["actions"]["oauth_client_id"]
        .as_str()
        .expect("Actions client id")
        .to_string();
    let source_gpt_config = run(&cli_args(
        &source_config,
        &[
            "workspace",
            "gpt-config",
            "migration-e2e",
            "--service",
            "all",
            "--local",
            "--show-secrets",
        ],
    ));
    assert!(
        source_gpt_config.status.success(),
        "source gpt-config failed: {}",
        String::from_utf8_lossy(&source_gpt_config.stderr)
    );

    let reserved_export = run(&cli_args(
        &source_config,
        &[
            "export",
            &source_profiles_path.to_string_lossy(),
            "--passphrase-file",
            &passphrase_file.to_string_lossy(),
            "--force",
        ],
    ));
    assert!(!reserved_export.status.success());
    assert!(String::from_utf8_lossy(&reserved_export.stderr).contains("不能覆盖 Anchor 当前配置"));
    assert_eq!(
        first_profile(&json_file(&source_profiles_path))["id"],
        workspace_id
    );

    let export = run(&cli_args(
        &source_config,
        &[
            "export",
            &bundle.to_string_lossy(),
            "--passphrase-file",
            &passphrase_file.to_string_lossy(),
        ],
    ));
    assert_success(&export);
    let export_report: Value = serde_json::from_slice(&export.stdout).expect("export report");
    assert_eq!(export_report["event"], "config_export");
    assert_eq!(export_report["registrationIdentityPreserved"], true);

    let bundle_text = fs::read_to_string(&bundle).expect("portable bundle");
    assert!(bundle_text.contains("argon2id-v1"));
    assert!(bundle_text.contains("aes-256-gcm-v1"));
    assert!(!bundle_text.contains(&workspace_id));
    assert!(!bundle_text.contains(&source_mcp_client_id));
    assert!(!bundle_text.contains(&source_actions_client_id));

    let tampered_bundle = temp.path().join("tampered-migration.json");
    let mut tampered: Value = serde_json::from_str(&bundle_text).expect("bundle json");
    tampered["sourcePlatform"] = Value::String("tampered-platform".into());
    fs::write(
        &tampered_bundle,
        serde_json::to_vec_pretty(&tampered).expect("tampered bundle json"),
    )
    .expect("tampered bundle");
    let tampered_import = run(&import_args(
        &temp.path().join("tampered-config"),
        &tampered_bundle,
        &passphrase_file,
        &workspace_id,
        &target_workspace,
    ));
    assert!(!tampered_import.status.success());
    assert!(String::from_utf8_lossy(&tampered_import.stderr).contains("解密失败"));

    let missing_target = temp.path().join("does-not-exist");
    let invalid_mapping = run(&import_args(
        &temp.path().join("validation-config"),
        &bundle,
        &passphrase_file,
        &workspace_id,
        &missing_target,
    ));
    assert!(!invalid_mapping.status.success());
    assert!(String::from_utf8_lossy(&invalid_mapping.stderr).contains("不存在或无法访问"));

    let relative_mapping = run(&import_args(
        &temp.path().join("relative-validation-config"),
        &bundle,
        &passphrase_file,
        &workspace_id,
        Path::new("relative/target"),
    ));
    assert!(!relative_mapping.status.success());
    assert!(String::from_utf8_lossy(&relative_mapping.stderr).contains("必须是绝对路径"));

    let wrong_passphrase = run(&import_args(
        &temp.path().join("wrong-pass-config"),
        &bundle,
        &wrong_passphrase_file,
        &workspace_id,
        &target_workspace,
    ));
    assert!(!wrong_passphrase.status.success());
    assert!(String::from_utf8_lossy(&wrong_passphrase.stderr).contains("解密失败"));

    let target_data = target_config.join("data");
    fs::create_dir_all(&target_data).expect("target data");
    fs::write(
        target_data.join("secrets.json"),
        r#"{
  "format": "anchor-secrets-envelope-v1",
  "version": 1,
  "protection": "windows-dpapi-current-user-v1",
  "payload": "AA=="
}
"#,
    )
    .expect("fake Windows DPAPI envelope");

    let before_import = run(&cli_args(&target_config, &["list"]));
    assert!(!before_import.status.success());
    assert!(
        String::from_utf8_lossy(&before_import.stderr)
            .contains("unsupported secret protection: windows-dpapi-current-user-v1"),
        "{}",
        output_text(&before_import)
    );

    let mut import = import_args(
        &target_config,
        &bundle,
        &passphrase_file,
        &workspace_id,
        &target_workspace,
    );
    import.push("--force".into());
    let imported = run(&import);
    assert_success(&imported);
    let import_report: Value = serde_json::from_slice(&imported.stdout).expect("import report");
    assert_eq!(import_report["event"], "config_import");
    assert_eq!(import_report["registrationIdentityPreserved"], true);
    assert_eq!(import_report["replacedExistingConfig"], true);
    assert_eq!(import_report["workspaces"][0]["workspaceId"], workspace_id);

    let target_profiles = json_file(&target_config.join("data/profiles.json"));
    let target_profile = first_profile(&target_profiles);
    assert_eq!(target_profile["id"], workspace_id);
    assert_eq!(
        target_profile["auth"]["oauth_client_id"],
        source_mcp_client_id
    );
    assert_eq!(
        target_profile["actions"]["oauth_client_id"],
        source_actions_client_id
    );
    assert_eq!(
        target_profile["path"],
        target_workspace
            .canonicalize()
            .expect("canonical target workspace")
            .to_string_lossy()
            .as_ref()
    );
    let mut normalized_target_profile = target_profile.clone();
    normalized_target_profile["path"] = source_profile["path"].clone();
    assert_eq!(normalized_target_profile, *source_profile);

    let target_secrets = json_file(&target_config.join("data/secrets.json"));
    #[cfg(windows)]
    assert_eq!(
        target_secrets["protection"],
        "windows-dpapi-current-user-v1"
    );
    #[cfg(not(windows))]
    assert_eq!(target_secrets["protection"], "private-file-permissions-v1");

    let after_import = run(&cli_args(&target_config, &["list"]));
    assert_success(&after_import);
    let list_output = String::from_utf8_lossy(&after_import.stdout);
    assert!(list_output.contains(&workspace_id));
    assert!(list_output.contains(target_workspace.to_string_lossy().as_ref()));

    let target_gpt_config = run(&cli_args(
        &target_config,
        &[
            "workspace",
            "gpt-config",
            "migration-e2e",
            "--service",
            "all",
            "--local",
            "--show-secrets",
        ],
    ));
    assert!(
        target_gpt_config.status.success(),
        "target gpt-config failed: {}",
        String::from_utf8_lossy(&target_gpt_config.stderr)
    );
    assert!(
        source_gpt_config.stdout == target_gpt_config.stdout,
        "迁移前后 ChatGPT/GPT 注册认证材料发生变化"
    );

    let import_without_force = run(&import_args(
        &target_config,
        &bundle,
        &passphrase_file,
        &workspace_id,
        &target_workspace,
    ));
    assert!(!import_without_force.status.success());
    assert!(String::from_utf8_lossy(&import_without_force.stderr).contains("目标配置已存在"));

    let mut dry_run = import_args(
        &target_config,
        &bundle,
        &passphrase_file,
        &workspace_id,
        &target_workspace,
    );
    dry_run.push("--dry-run".into());
    let dry_run = run(&dry_run);
    assert_success(&dry_run);
    let dry_run_report: Value = serde_json::from_slice(&dry_run.stdout).expect("dry run report");
    assert_eq!(dry_run_report["dryRun"], true);
    assert_eq!(dry_run_report["replacedExistingConfig"], false);
}

#[test]
fn config_migration_cli_rejects_conflicting_passphrase_sources() {
    let temp = tempfile::tempdir().expect("migration args tempdir");
    let config = temp.path().join("config");
    let bundle = temp.path().join("bundle.json");
    let passphrase = temp.path().join("passphrase");
    fs::write(&passphrase, b"portable-migration-passphrase").expect("passphrase");

    let conflicting = run(&cli_args(
        &config,
        &[
            "config",
            "export",
            &bundle.to_string_lossy(),
            "--passphrase-file",
            &passphrase.to_string_lossy(),
            "--passphrase-stdin",
        ],
    ));
    assert!(!conflicting.status.success());
    assert!(String::from_utf8_lossy(&conflicting.stderr).contains("只能选择一种"));
}
