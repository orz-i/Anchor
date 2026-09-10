use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read directory") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[test]
fn actions_runtime_and_web_entrypoints_are_physically_absent() {
    let root = repository_root();
    for path in [
        "crates/anchor/src/actions",
        "src/components/admin/ActionsAuthForm.tsx",
        "src/components/admin/ActionsPolicyForm.tsx",
        "src/pages/WorkspacePage.tsx",
    ] {
        assert!(
            !root.join(path).exists(),
            "retired Actions path remains: {path}"
        );
    }
}

#[test]
fn active_source_has_no_actions_runtime_contracts() {
    let root = repository_root();
    let mut files = Vec::new();
    collect_files(&root.join("crates/anchor/src"), &mut files);
    collect_files(&root.join("src"), &mut files);

    let migration_allowlist = [
        root.join("crates/anchor/src/data/store.rs"),
        root.join("crates/anchor/src/data/storage.rs"),
    ];
    let forbidden = [
        "ActionsConfig",
        "ACTIONS_SERVER_NAME",
        "ServiceSelection::Actions",
        "ServiceKind::Actions",
        "TunnelServiceKind::Actions",
        "ControlService::Actions",
        "WorkspaceService::Actions",
        "get_actions_runtime_status",
        "start_actions_runtime",
        "stop_actions_runtime",
        "restart_actions_runtime",
        "actions_local_base_url",
        "actions_effective_public_url",
        "actions_public_base_url",
        "/actions/",
        "openapi.json",
    ];
    let retired_secret_tokens = [
        "actions_api_key",
        "actions_oauth_client_secret",
        "actions_oauth_password",
        "actions_oauth_token_secret",
        "actions_cloudflare_token",
        "actions_frp_token",
    ];

    let mut violations = Vec::new();
    for path in files {
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read active source");
        for token in forbidden {
            if source.contains(token) {
                violations.push(format!("{}: {token}", path.display()));
            }
        }
        if !migration_allowlist.contains(&path) {
            for token in retired_secret_tokens {
                if source.contains(token) {
                    violations.push(format!("{}: {token}", path.display()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "active Actions runtime contracts remain: {violations:?}"
    );
}

#[test]
fn current_product_docs_have_no_actions_setup_or_runtime_instructions() {
    let root = repository_root();
    let mut files = vec![root.join("README.md"), root.join("README.en.md")];
    for path in [
        "docs/cli-daemon.md",
        "docs/config-migration.md",
        "docs/linux-cli.md",
        "docs/mcp-gateway.md",
        "docs/project-context.md",
        "docs/project-context/architecture.md",
        "docs/reliability.md",
        "docs/workspace-cli.md",
    ] {
        files.push(root.join(path));
    }

    let forbidden = [
        "GPT Actions",
        "ChatGPT Actions",
        "Actions 服务",
        "Actions service",
        "Actions 端口",
        "actionsPort",
        "--service actions",
        "actionsState",
        "actions_api_key",
        "openapi.json",
    ];
    let mut violations = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path).expect("read current product docs");
        for token in forbidden {
            if source.contains(token) {
                violations.push(format!("{}: {token}", path.display()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "current documentation still exposes Actions: {violations:?}"
    );
}

#[test]
fn legacy_actions_data_is_only_kept_in_the_unversioned_storage_migration() {
    let root = repository_root();
    let storage = fs::read_to_string(root.join("crates/anchor/src/data/storage.rs"))
        .expect("read profile migration");
    let store =
        fs::read_to_string(root.join("crates/anchor/src/data/store.rs")).expect("read data store");

    assert!(storage.contains("profile.remove(\"actions\")"));
    assert!(storage.contains("LEGACY_V0_ACTIONS_SECRET_KEYS"));
    assert!(storage.contains("migrate_unversioned_secrets"));
    assert!(!store.contains("RETIRED_ACTIONS_SECRET_KEYS"));
    assert!(!store.contains("strip_retired_actions_secrets"));
}
