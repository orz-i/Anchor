use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::skills::{PluginSkillExport, SkillCatalog, SkillSettings};
use crate::workspace::WorkspaceProfile;

use super::args::{PluginCommand, PluginPackageOptions};

const DEFAULT_MARKETPLACE_DIR: &str = ".anchor/chatgpt-plugin-marketplace";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginPackageResult {
    plugin_name: String,
    workspace_id: String,
    workspace_name: String,
    marketplace_root: String,
    plugin_root: String,
    app_id_prefix: String,
    skill_count: usize,
    skills: Vec<String>,
    skill_catalog_digest: String,
    skill_bytes: u64,
    warnings: Vec<String>,
    next_steps: Vec<String>,
}

pub async fn execute(command: PluginCommand, as_json: bool) -> AppResult<i32> {
    match command {
        PluginCommand::Package(options) => {
            let store = DataStore::load()?;
            let profile = super::resolve_workspace(store.list(), &options.workspace)?.clone();
            drop(store);
            let result = package_profile(&profile, &options)?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result)
                        .map_err(|error| AppError::Message(error.to_string()))?
                );
            } else {
                println!("ChatGPT Plugin package 已生成：{}", result.plugin_root);
                println!(
                    "Skills: {} ({})",
                    result.skill_count,
                    result.skills.join(", ")
                );
                println!("Marketplace: {}", result.marketplace_root);
                for warning in &result.warnings {
                    eprintln!("warning: {warning}");
                }
                println!("下一步：");
                for (index, step) in result.next_steps.iter().enumerate() {
                    println!("  {}. {}", index + 1, step);
                }
            }
            Ok(0)
        }
    }
}

fn package_profile(
    profile: &WorkspaceProfile,
    options: &PluginPackageOptions,
) -> AppResult<PluginPackageResult> {
    validate_app_id(&options.app_id)?;
    let workspace_root = PathBuf::from(&profile.path);
    if !workspace_root.is_dir() {
        return Err(AppError::Message(format!(
            "workspace 目录不存在：{}",
            workspace_root.display()
        )));
    }

    let plugin_name = match options.name.as_deref() {
        Some(name) => validate_plugin_name(name)?.to_string(),
        None => default_plugin_name(&profile.name),
    };
    let marketplace_root = match &options.output {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => workspace_root.join(path),
        None => workspace_root.join(DEFAULT_MARKETPLACE_DIR),
    };
    let plugins_root = marketplace_root.join("plugins");
    let plugin_root = plugins_root.join(&plugin_name);
    let staging_root = plugins_root.join(format!(".{plugin_name}.staging-{}", std::process::id()));

    if staging_root.exists() {
        fs::remove_dir_all(&staging_root).map_err(io_error("清理 Plugin staging 目录"))?;
    }
    fs::create_dir_all(staging_root.join(".codex-plugin"))
        .map_err(io_error("创建 .codex-plugin 目录"))?;

    let catalog = SkillCatalog::new(workspace_root.clone());
    catalog.configure(SkillSettings::from_text(
        profile.runtime.skill_service_enabled,
        &profile.runtime.skill_roots,
    ));
    let skill_export = catalog
        .export_plugin_skills(&staging_root.join("skills"))
        .map_err(AppError::Message)?;
    let display_name = if profile.name.eq_ignore_ascii_case("anchor") {
        "Anchor".to_string()
    } else {
        format!("Anchor · {}", profile.name)
    };

    write_json(
        &staging_root.join(".codex-plugin/plugin.json"),
        &json!({
            "name": plugin_name,
            "version": env!("CARGO_PKG_VERSION"),
            "description": format!("Anchor MCP tools and Workspace Skills for {}.", profile.name),
            "skills": "./skills/",
            "apps": "./.app.json",
            "interface": {
                "displayName": display_name,
                "shortDescription": "Workspace workflows backed by the Anchor MCP app",
                "developerName": "Anchor"
            }
        }),
    )?;
    write_json(
        &staging_root.join(".app.json"),
        &json!({
            "apps": {
                "anchor": {
                    "id": options.app_id
                }
            }
        }),
    )?;

    fs::create_dir_all(&plugins_root).map_err(io_error("创建 Plugin marketplace 目录"))?;
    if plugin_root.exists() {
        fs::remove_dir_all(&plugin_root).map_err(io_error("替换旧 Plugin package"))?;
    }
    fs::rename(&staging_root, &plugin_root).map_err(io_error("发布 Plugin package"))?;

    write_json(
        &marketplace_root.join("marketplace.json"),
        &json!({
            "name": format!("anchor-local-{}", profile.id.chars().take(8).collect::<String>()),
            "plugins": [
                {
                    "name": plugin_name,
                    "source": {
                        "source": "local",
                        "path": format!("./plugins/{plugin_name}")
                    },
                    "policy": {
                        "installation": "AVAILABLE",
                        "authentication": "ON_INSTALL"
                    },
                    "category": "Productivity"
                }
            ]
        }),
    )?;

    Ok(build_result(
        profile,
        plugin_name,
        marketplace_root,
        plugin_root,
        skill_export,
    ))
}

fn build_result(
    profile: &WorkspaceProfile,
    plugin_name: String,
    marketplace_root: PathBuf,
    plugin_root: PathBuf,
    skill_export: PluginSkillExport,
) -> PluginPackageResult {
    let marketplace = display_path(&marketplace_root);
    PluginPackageResult {
        plugin_name,
        workspace_id: profile.id.clone(),
        workspace_name: profile.name.clone(),
        marketplace_root: marketplace.clone(),
        plugin_root: display_path(&plugin_root),
        app_id_prefix: "plugin_asdk_app".into(),
        skill_count: skill_export.skills.len(),
        skills: skill_export.skills,
        skill_catalog_digest: skill_export.catalog_digest,
        skill_bytes: skill_export.total_bytes,
        warnings: skill_export.warnings,
        next_steps: vec![
            format!("运行 `codex plugin marketplace add \"{marketplace}\"` 注册本地 marketplace"),
            "重启 ChatGPT desktop app".into(),
            "在 Plugins Directory 选择该 local marketplace，并安装生成的 Anchor plugin".into(),
            "新建聊天后打开插件详情，确认 Skills 列表并测试 Skill activation".into(),
        ],
    }
}

fn validate_app_id(value: &str) -> AppResult<()> {
    let value = value.trim();
    if !value.starts_with("plugin_asdk_app")
        || value.len() <= "plugin_asdk_app".len()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AppError::Message(
            "--app-id 必须是 ChatGPT Developer mode 注册 MCP app 后浏览器 URL 中的 plugin_asdk_app... technical ID".into(),
        ));
    }
    Ok(())
}

fn validate_plugin_name(value: &str) -> AppResult<&str> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 96
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(AppError::Message(
            "--name 必须是稳定的 kebab-case plugin name（小写 ASCII 字母、数字和单个连字符）"
                .into(),
        ));
    }
    Ok(value)
}

fn default_plugin_name(workspace_name: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in workspace_name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            pending_dash = false;
        } else if !slug.is_empty() {
            pending_dash = true;
        }
    }
    if slug.is_empty() || slug == "anchor" {
        "anchor".into()
    } else {
        format!("anchor-{slug}")
    }
}

fn write_json(path: &Path, value: &serde_json::Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error("创建 JSON 父目录"))?;
    }
    let mut content = serde_json::to_string_pretty(value)
        .map_err(|error| AppError::Message(error.to_string()))?;
    content.push('\n');
    fs::write(path, content).map_err(io_error("写入 Plugin JSON"))
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn io_error(context: &'static str) -> impl FnOnce(std::io::Error) -> AppError {
    move |error| AppError::Message(format!("{context}失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_profile(root: &Path) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(
            root.to_string_lossy().to_string(),
            Some("Anchor Demo".into()),
        );
        profile.runtime.skill_service_enabled = true;
        profile.runtime.skill_roots = ".agents/skills".into();
        profile
    }

    fn write_fixture_skill(root: &Path) {
        let skill = root.join(".agents/skills/review-anchor");
        fs::create_dir_all(skill.join("references")).expect("skill dir");
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: review-anchor\ndescription: Review Anchor changes.\n---\nReview the requested Anchor change.\n",
        )
        .expect("skill md");
        fs::write(skill.join("references/checks.md"), "Run checks.\n").expect("reference");
    }

    #[test]
    fn packages_workspace_skills_as_real_plugin_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_fixture_skill(temp.path());
        let profile = fixture_profile(temp.path());
        let output = temp.path().join("marketplace");
        let options = PluginPackageOptions {
            workspace: profile.id.clone(),
            app_id: "plugin_asdk_app_test123".into(),
            output: Some(output.clone()),
            name: None,
        };

        let result = package_profile(&profile, &options).expect("package");
        assert_eq!(result.plugin_name, "anchor-anchor-demo");
        assert_eq!(result.skills, vec!["review-anchor"]);
        let plugin = output.join("plugins/anchor-anchor-demo");
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(plugin.join(".codex-plugin/plugin.json")).expect("manifest"),
        )
        .expect("manifest json");
        assert_eq!(manifest["skills"], "./skills/");
        assert_eq!(manifest["apps"], "./.app.json");
        let app: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(plugin.join(".app.json")).expect("app"))
                .expect("app json");
        assert_eq!(app["apps"]["anchor"]["id"], "plugin_asdk_app_test123");
        assert!(plugin.join("skills/review-anchor/SKILL.md").is_file());
        assert!(plugin
            .join("skills/review-anchor/references/checks.md")
            .is_file());
        let marketplace: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output.join("marketplace.json")).expect("marketplace"),
        )
        .expect("marketplace json");
        assert_eq!(
            marketplace["plugins"][0]["source"]["path"],
            "./plugins/anchor-anchor-demo"
        );
    }

    #[test]
    fn rejects_non_chatgpt_app_ids_and_invalid_plugin_names() {
        assert!(validate_app_id("connector_123").is_err());
        assert!(validate_app_id("plugin_asdk_app_test123").is_ok());
        assert!(validate_plugin_name("anchor-demo").is_ok());
        assert!(validate_plugin_name("Anchor Demo").is_err());
    }
}
