mod catalog;
mod model;
mod resource;

use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};

pub use catalog::{SkillCatalog, SkillSettings};

pub const TOOL_NAMES: &[&str] = &["list_skills", "load_skill", "read_skill_resource"];

pub fn list_tool(catalog: &SkillCatalog, args: &Value) -> Result<Value, WorkspaceError> {
    let query = args.get("query").and_then(Value::as_str);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(100) as usize;
    Ok(tool_ok(
        serde_json::to_value(catalog.list(query, max_results)).map_err(skill_error)?,
    ))
}

pub fn load_tool(catalog: &SkillCatalog, args: &Value) -> Result<Value, WorkspaceError> {
    let name = required_string(args, "name")?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144);
    let loaded = catalog.load(name, max_bytes).map_err(skill_not_found_or_invalid)?;
    Ok(tool_ok(serde_json::to_value(loaded).map_err(skill_error)?))
}

pub fn read_resource_tool(
    catalog: &SkillCatalog,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let name = required_string(args, "name")?;
    let path = required_string(args, "path")?;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144);
    let resource = catalog
        .read_resource(name, path, start_line, end_line, max_bytes)
        .map_err(skill_not_found_or_invalid)?;
    Ok(tool_ok(
        serde_json::to_value(resource).map_err(skill_error)?,
    ))
}

pub fn resources_list(catalog: &SkillCatalog) -> Value {
    if !catalog.is_enabled() {
        return json!({ "resources": [] });
    }
    let listed = catalog.list(None, 200);
    let mut resources = vec![json!({
        "uri": "skill://index.json",
        "name": "Agent Skills index",
        "title": "Agent Skills discovery index",
        "description": "Progressive-disclosure index for skills exposed by this workspace/profile.",
        "mimeType": "application/json"
    })];
    for skill in listed.skills {
        resources.push(json!({
            "uri": skill.uri,
            "name": skill.name,
            "title": skill.name,
            "description": skill.description,
            "mimeType": "text/markdown"
        }));
        for item in skill.resources.iter().chain(skill.scripts.iter()) {
            resources.push(json!({
                "uri": format!("skill://{}/{}", skill.name, item.path),
                "name": format!("{}/{}", skill.name, item.path),
                "title": item.path,
                "description": if item.kind == "script" {
                    "Skill script source; execution is disabled by this server."
                } else {
                    "Skill supporting resource."
                },
                "mimeType": item.mime_type
            }));
        }
    }
    json!({ "resources": resources })
}

pub fn resource_read(catalog: &SkillCatalog, params: &Value) -> Result<Value, Value> {
    if !catalog.is_enabled() {
        return Err(rpc_error(
            -32602,
            "Skill service is disabled for this workspace/profile",
        ));
    }
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_error(-32602, "Missing resource uri"))?;
    if uri == "skill://index.json" {
        let text = catalog
            .index_json()
            .map_err(|error| rpc_error(-32603, &error))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text
            }]
        }));
    }

    let (name, path) = parse_skill_uri(uri)
        .ok_or_else(|| rpc_error(-32602, "Skill resource URI must use skill://<name>/<path>"))?;
    if path == "SKILL.md" {
        let loaded = catalog
            .load(name, 1_048_576)
            .map_err(|error| rpc_error(-32002, &error))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": loaded.skill_md
            }]
        }));
    }

    let resource = catalog
        .read_resource(name, path, None, None, 1_048_576)
        .map_err(|error| rpc_error(-32002, &error))?;
    let content = if resource.encoding == "base64" {
        json!({
            "uri": uri,
            "mimeType": resource.mime_type,
            "blob": resource.content
        })
    } else {
        json!({
            "uri": uri,
            "mimeType": resource.mime_type,
            "text": resource.content
        })
    };
    Ok(json!({ "contents": [content] }))
}

pub fn is_skill_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument(format!("{key} is required")))
}

fn skill_error(error: serde_json::Error) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "SKILL_SERIALIZATION_FAILED",
        message: error.to_string(),
        category: "internal",
        retryable: false,
    }
}

fn skill_not_found_or_invalid(message: String) -> WorkspaceError {
    let code = if message.starts_with("找不到 Skill") {
        "SKILL_NOT_FOUND"
    } else if message.contains("未启用") {
        "SKILL_SERVICE_DISABLED"
    } else {
        "SKILL_RESOURCE_INVALID"
    };
    WorkspaceError::Tool {
        code,
        message,
        category: if code == "SKILL_NOT_FOUND" {
            "not_found"
        } else {
            "validation"
        },
        retryable: false,
    }
}

fn parse_skill_uri(uri: &str) -> Option<(&str, &str)> {
    let remainder = uri.strip_prefix("skill://")?;
    let (name, path) = remainder.split_once('/')?;
    if name.is_empty() || path.is_empty() {
        return None;
    }
    Some((name, path))
}

fn rpc_error(code: i32, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn catalog_with_skill() -> (tempfile::TempDir, SkillCatalog) {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("skills/example");
        fs::create_dir_all(skill_dir.join("references")).expect("create skill");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: example\ndescription: Example skill.\n---\nUse this skill.\n",
        )
        .expect("write skill");
        fs::write(skill_dir.join("references/INFO.md"), "info").expect("write info");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        (temp, catalog)
    }

    #[test]
    fn index_and_resources_use_skill_uri_scheme() {
        let (_temp, catalog) = catalog_with_skill();

        let index: Value = serde_json::from_str(&catalog.index_json().expect("index"))
            .expect("index json");
        assert_eq!(index["skills"][0]["type"], "skill-md");
        assert_eq!(index["skills"][0]["url"], "skill://example/SKILL.md");

        let listed = resources_list(&catalog);
        assert!(listed["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .any(|item| item["uri"] == "skill://example/references/INFO.md"));
    }

    #[test]
    fn resource_read_returns_skill_markdown() {
        let (_temp, catalog) = catalog_with_skill();

        let result = resource_read(&catalog, &json!({"uri": "skill://example/SKILL.md"}))
            .expect("read resource");

        assert!(result["contents"][0]["text"]
            .as_str()
            .expect("text")
            .contains("Use this skill"));
    }

    #[test]
    fn disabled_service_exposes_no_resources() {
        let (_temp, catalog) = catalog_with_skill();
        catalog.configure(SkillSettings::from_text(false, "skills"));

        assert_eq!(resources_list(&catalog)["resources"], json!([]));
        let error = resource_read(&catalog, &json!({"uri": "skill://index.json"}))
            .expect_err("disabled resource read");
        assert_eq!(error["code"], -32602);
    }
}
