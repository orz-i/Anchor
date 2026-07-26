mod catalog;
mod model;
mod resource;

use base64::Engine;
use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::ToolContext;

pub use catalog::{SkillCatalog, SkillSettings};

pub const TOOL_NAMES: &[&str] = &["list_skills", "load_skill", "read_skill_resource"];
const RESOURCE_PAGE_SIZE: usize = 200;
const MAX_RESOURCE_ENTRIES: usize = 5000;

pub fn list_tool(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let query = args.get("query").and_then(Value::as_str);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(100) as usize;
    let mut listed = ctx.skills.list(query, max_results);
    let available = available_tools(ctx);
    for skill in &mut listed.skills {
        skill.resolve_tools(&available);
        if !skill.tool_compatible {
            skill.warnings.push(format!(
                "工具依赖不完整：missing={:?}, ambiguous={:?}",
                skill.missing_tools, skill.ambiguous_tools
            ));
        }
    }
    Ok(tool_ok(serde_json::to_value(listed).map_err(skill_error)?))
}

pub fn load_tool(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let name = required_string(args, "name")?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536);
    let mut loaded = ctx
        .skills
        .load(name, max_bytes)
        .map_err(skill_not_found_or_invalid)?;
    loaded.summary.resolve_tools(&available_tools(ctx));
    if !loaded.summary.tool_compatible {
        loaded.summary.warnings.push(format!(
            "当前 MCP 工具目录不能完整满足该 Skill：missing={:?}, ambiguous={:?}",
            loaded.summary.missing_tools, loaded.summary.ambiguous_tools
        ));
    }
    Ok(tool_ok(serde_json::to_value(loaded).map_err(skill_error)?))
}

pub fn read_resource_tool(catalog: &SkillCatalog, args: &Value) -> Result<Value, WorkspaceError> {
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

pub fn resources_list(catalog: &SkillCatalog, params: &Value) -> Result<Value, Value> {
    if !catalog.is_enabled() {
        return Ok(json!({ "resources": [] }));
    }
    let offset = params
        .get("cursor")
        .and_then(Value::as_str)
        .map(decode_cursor)
        .transpose()
        .map_err(|error| rpc_error(-32602, &error))?
        .unwrap_or(0);
    let listed = catalog.list(None, 200);
    let catalog_truncated =
        listed.truncated || listed.skills.iter().any(|skill| skill.resource_truncated);
    let mut resources = vec![json!({
        "uri": "skill://index.json",
        "name": "Agent Skills index",
        "title": "Agent Skills discovery index",
        "description": "Snapshot discovery index for skills exposed by this workspace/profile.",
        "mimeType": "application/json"
    })];
    for skill in listed.skills {
        if resources.len() >= MAX_RESOURCE_ENTRIES {
            break;
        }
        resources.push(json!({
            "uri": skill.uri,
            "name": skill.name,
            "title": skill.name,
            "description": skill.description,
            "mimeType": "text/markdown"
        }));
        for item in skill.resources.iter().chain(skill.scripts.iter()) {
            if resources.len() >= MAX_RESOURCE_ENTRIES {
                break;
            }
            resources.push(json!({
                "uri": resource::skill_resource_uri(&skill.name, &item.path),
                "name": format!("{}/{}", skill.name, item.path),
                "title": item.path,
                "description": if item.kind == "script" {
                    "Skill script source. Direct Skill execution is disabled; generic exec_command requires operator-enabled dangerous mode."
                } else {
                    "Skill supporting resource."
                },
                "mimeType": item.mime_type,
                "annotations": {
                    "audience": ["assistant"],
                    "priority": if item.kind == "script" { 0.3 } else { 0.5 }
                }
            }));
        }
    }
    if offset > resources.len() {
        return Err(rpc_error(-32602, "Invalid resources/list cursor"));
    }
    let end = (offset + RESOURCE_PAGE_SIZE).min(resources.len());
    let page = resources[offset..end].to_vec();
    let mut result = json!({
        "resources": page,
        "_meta": {
            "snapshotMode": listed.snapshot_mode,
            "catalogDigest": listed.catalog_digest,
            "catalogTruncated": catalog_truncated || resources.len() >= MAX_RESOURCE_ENTRIES,
            "maximumResourceEntries": MAX_RESOURCE_ENTRIES
        }
    });
    if end < resources.len() {
        result["nextCursor"] = Value::String(encode_cursor(end));
    }
    Ok(result)
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

    let (name, path) = parse_skill_uri(uri).map_err(|error| rpc_error(-32602, &error))?;
    if path == "SKILL.md" {
        let skill_md = catalog
            .skill_markdown(&name)
            .map_err(|error| rpc_error(-32002, &error))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": skill_md
            }]
        }));
    }

    let resource = catalog
        .read_resource(&name, &path, None, None, 1_048_576)
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

fn available_tools(ctx: &ToolContext) -> Vec<String> {
    let mut tools = crate::tools::registry::exposed_tool_names(&ctx.tool_profile)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    tools.extend(
        ctx.mcp_proxies
            .list_tools()
            .into_iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string)),
    );
    tools.sort();
    tools.dedup();
    tools
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

fn parse_skill_uri(uri: &str) -> Result<(String, String), String> {
    if uri.contains(['?', '#']) {
        return Err("Skill resource URI must not contain query or fragment".into());
    }
    let remainder = uri
        .strip_prefix("skill://")
        .ok_or_else(|| "Skill resource URI must use skill://<name>/<path>".to_string())?;
    let (name, encoded_path) = remainder
        .split_once('/')
        .ok_or_else(|| "Skill resource URI must use skill://<name>/<path>".to_string())?;
    if name.is_empty() || encoded_path.is_empty() {
        return Err("Skill resource URI must use skill://<name>/<path>".into());
    }
    let path = resource::percent_decode_path(encoded_path)?;
    Ok((name.to_string(), path))
}

fn encode_cursor(offset: usize) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(offset.to_string())
}

fn decode_cursor(cursor: &str) -> Result<usize, String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "Invalid resources/list cursor".to_string())?;
    let value =
        String::from_utf8(bytes).map_err(|_| "Invalid resources/list cursor".to_string())?;
    value
        .parse::<usize>()
        .map_err(|_| "Invalid resources/list cursor".to_string())
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

        let index: Value =
            serde_json::from_str(&catalog.index_json().expect("index")).expect("index json");
        assert_eq!(index["skills"][0]["type"], "skill-md");
        assert_eq!(index["skills"][0]["url"], "skill://example/SKILL.md");

        let listed = resources_list(&catalog, &json!({})).expect("resources");
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

        assert_eq!(
            resources_list(&catalog, &json!({})).unwrap()["resources"],
            json!([])
        );
        let error = resource_read(&catalog, &json!({"uri": "skill://index.json"}))
            .expect_err("disabled resource read");
        assert_eq!(error["code"], -32602);
    }

    #[test]
    fn resources_list_supports_opaque_cursor_pagination() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..205 {
            let skill_dir = temp.path().join(format!("skills/s{index}"));
            fs::create_dir_all(&skill_dir).expect("skill dir");
            fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: s{index}\ndescription: Skill {index}.\n---\nUse it.\n"),
            )
            .expect("skill");
        }
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        let first = resources_list(&catalog, &json!({})).expect("first page");
        assert_eq!(first["resources"].as_array().unwrap().len(), 200);
        let cursor = first["nextCursor"].as_str().expect("cursor");
        let second = resources_list(&catalog, &json!({"cursor": cursor})).expect("second");
        assert!(!second["resources"].as_array().unwrap().is_empty());
    }
}
