mod catalog;
mod model;
mod resource;

use base64::Engine;
use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::ToolContext;

pub use catalog::{SkillCatalog, SkillSettings};

pub const TOOL_NAMES: &[&str] = &[
    "list_skills",
    "load_skill",
    "list_skill_resources",
    "read_skill_resource",
];
const RESOURCE_PAGE_SIZE: usize = 200;
const MAX_RESOURCE_ENTRIES: usize = 5000;
const DEFAULT_SKILL_PAGE_BYTES: u64 = 65_536;
const NATIVE_SKILL_PAGE_SIZE: usize = 5;

#[derive(Debug)]
struct SkillUriRequest {
    name: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: u64,
}

pub fn list_resources_tool(catalog: &SkillCatalog, args: &Value) -> Result<Value, WorkspaceError> {
    let name = required_string(args, "name")?;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let listed = catalog.list(None, 1_000);
    let skill = listed
        .skills
        .into_iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| skill_not_found_or_invalid(format!("找不到 Skill: {name}")))?;
    let mut resources = skill
        .resources
        .iter()
        .chain(skill.scripts.iter())
        .map(|item| {
            json!({
                "path": item.path,
                "kind": item.kind,
                "sizeBytes": item.size_bytes,
                "mimeType": item.mime_type,
                "readable": item.readable,
                "digest": item.digest,
                "uri": resource::skill_resource_uri(&skill.name, &item.path),
                "readArgs": if item.readable {
                    json!({"name": skill.name, "path": item.path})
                } else {
                    Value::Null
                }
            })
        })
        .collect::<Vec<_>>();
    resources.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    if cursor > resources.len() {
        return Err(WorkspaceError::invalid_argument(
            "cursor exceeds the readable resource manifest",
        ));
    }

    let end = (cursor + limit).min(resources.len());
    let page = resources[cursor..end].to_vec();
    Ok(tool_ok(json!({
        "skill": skill.name,
        "resources": page,
        "totalResources": resources.len(),
        "nextCursor": if end < resources.len() { Some(end) } else { None },
        "resourceTruncated": skill.resource_truncated,
        "snapshotMode": listed.snapshot_mode,
        "catalogDigest": listed.catalog_digest,
        "warnings": listed.warnings
    })))
}

pub fn list_tool(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let query = args.get("query").and_then(Value::as_str);
    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(100) as usize;
    let mut listed = ctx.skills.list(query, max_results);
    let available = available_tools(ctx)?;
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
        .unwrap_or(DEFAULT_SKILL_PAGE_BYTES);
    let mut loaded = ctx
        .skills
        .load(name, start_line, end_line, max_bytes)
        .map_err(skill_not_found_or_invalid)?;
    loaded.summary.resolve_tools(&available_tools(ctx)?);
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

pub fn native_skills_list(catalog: &SkillCatalog, params: &Value) -> Result<Value, Value> {
    if !catalog.is_enabled() {
        return Err(rpc_error(
            -32602,
            "Skill service is disabled for this workspace/profile",
        ));
    }
    let offset = params
        .get("cursor")
        .and_then(Value::as_str)
        .map(decode_cursor)
        .transpose()
        .map_err(|error| rpc_error(-32602, &error))?
        .unwrap_or(0);
    let native = catalog.native_catalog();
    if offset > native.skills.len() {
        return Err(rpc_error(-32602, "Invalid skills/list cursor"));
    }
    let end = (offset + NATIVE_SKILL_PAGE_SIZE).min(native.skills.len());
    let page = native.skills[offset..end].to_vec();
    let mut result = json!({ "skills": page });
    if end < native.skills.len() {
        result["nextCursor"] = Value::String(encode_cursor(end));
    }
    Ok(result)
}

pub fn native_skill_get(catalog: &SkillCatalog, params: &Value) -> Result<Value, Value> {
    if !catalog.is_enabled() {
        return Err(rpc_error(
            -32602,
            "Skill service is disabled for this workspace/profile",
        ));
    }
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_error(-32602, "Missing skill uri"))?;
    if uri.contains('?') || uri.contains('#') {
        return Err(rpc_error(
            -32602,
            "skills/get requires the canonical SKILL.md URI without query or fragment",
        ));
    }
    let request = parse_skill_uri(uri).map_err(|error| rpc_error(-32602, &error))?;
    if request.path != "SKILL.md" {
        return Err(rpc_error(-32602, "skills/get URI must point to SKILL.md"));
    }
    let skill = catalog
        .native_catalog()
        .skills
        .into_iter()
        .find(|skill| skill["uri"] == uri)
        .ok_or_else(|| rpc_error(-32002, "Skill is not exposed by skills/list"))?;
    Ok(json!({ "skill": skill }))
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

    let request = parse_skill_uri(uri).map_err(|error| rpc_error(-32602, &error))?;
    if request.path == "SKILL.md" {
        let skill_md = catalog
            .read_skill_markdown(
                &request.name,
                request.start_line,
                request.end_line,
                request.max_bytes,
            )
            .map_err(|error| rpc_error(-32002, &error))?;
        let mut result = json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": skill_md.content
            }],
            "_meta": {
                "startLine": skill_md.start_line,
                "endLine": skill_md.end_line,
                "totalLines": skill_md.total_lines,
                "totalBytes": skill_md.size_bytes,
                "returnedBytes": skill_md.returned_bytes,
                "truncated": skill_md.truncated
            }
        });
        if let Some(next_start_line) = skill_md.next_start_line {
            result["_meta"]["nextStartLine"] = json!(next_start_line);
            result["_meta"]["nextUri"] = Value::String(format!(
                "skill://{}/SKILL.md?start_line={next_start_line}&max_bytes={}",
                request.name, request.max_bytes
            ));
        }
        return Ok(result);
    }

    let resource = catalog
        .read_resource(&request.name, &request.path, None, None, 1_048_576)
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

fn available_tools(ctx: &ToolContext) -> Result<Vec<String>, WorkspaceError> {
    Ok(crate::tools::catalog::build_effective_catalog(ctx)?
        .tools
        .into_iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect())
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

fn parse_skill_uri(uri: &str) -> Result<SkillUriRequest, String> {
    if uri.contains('#') {
        return Err("Skill resource URI must not contain a fragment".into());
    }
    let (base_uri, query) = uri
        .split_once('?')
        .map_or((uri, None), |(base, query)| (base, Some(query)));
    let remainder = base_uri
        .strip_prefix("skill://")
        .ok_or_else(|| "Skill resource URI must use skill://<name>/<path>".to_string())?;
    let (name, encoded_path) = remainder
        .split_once('/')
        .ok_or_else(|| "Skill resource URI must use skill://<name>/<path>".to_string())?;
    if name.is_empty() || encoded_path.is_empty() {
        return Err("Skill resource URI must use skill://<name>/<path>".into());
    }
    let path = resource::percent_decode_path(encoded_path)?;
    let mut request = SkillUriRequest {
        name: name.to_string(),
        path,
        start_line: None,
        end_line: None,
        max_bytes: catalog::MAX_SKILL_MD_BYTES,
    };
    let Some(query) = query else {
        return Ok(request);
    };
    if request.path != "SKILL.md" {
        return Err("Skill resource query parameters are supported only for SKILL.md".into());
    }
    let mut seen = std::collections::HashSet::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| "Skill resource query parameters must use key=value".to_string())?;
        if !seen.insert(key) {
            return Err(format!("Duplicate Skill resource query parameter: {key}"));
        }
        match key {
            "start_line" => {
                request.start_line = Some(parse_positive_query_usize(key, value)?);
            }
            "end_line" => {
                request.end_line = Some(parse_positive_query_usize(key, value)?);
            }
            "max_bytes" => {
                let parsed = parse_positive_query_usize(key, value)? as u64;
                if parsed > catalog::MAX_SKILL_MD_BYTES {
                    return Err(format!(
                        "max_bytes cannot exceed {}",
                        catalog::MAX_SKILL_MD_BYTES
                    ));
                }
                request.max_bytes = parsed;
            }
            _ => return Err(format!("Unknown Skill resource query parameter: {key}")),
        }
    }
    if request
        .end_line
        .zip(request.start_line)
        .is_some_and(|(end, start)| end < start)
    {
        return Err("end_line cannot be smaller than start_line".into());
    }
    Ok(request)
}

fn parse_positive_query_usize(name: &str, value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be a positive integer"))?;
    if value == 0 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(value)
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
    fn native_list_uses_only_standard_fields_and_get_is_limited_to_exposed_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..6 {
            let skill_dir = temp.path().join(format!("skills/skill-{index}"));
            fs::create_dir_all(&skill_dir).expect("skill dir");
            fs::write(
                skill_dir.join("SKILL.md"),
                format!(
                    "---\nname: skill-{index}\ndescription: Native skill {index}.\n---\nUse it.\n"
                ),
            )
            .expect("skill");
        }
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let listed = native_skills_list(&catalog, &json!({})).expect("native list");
        let object = listed.as_object().expect("list object");
        assert_eq!(object.len(), 1);
        assert!(object.contains_key("skills"));
        assert_eq!(listed["skills"].as_array().expect("skills").len(), 5);

        let exposed = native_skill_get(&catalog, &json!({"uri": "skill://skill-4/SKILL.md"}))
            .expect("exposed get");
        assert_eq!(exposed["skill"]["frontmatter"]["name"], "skill-4");

        let omitted = native_skill_get(&catalog, &json!({"uri": "skill://skill-5/SKILL.md"}))
            .expect_err("omitted skill must not be gettable");
        assert_eq!(omitted["code"], -32002);
    }

    #[test]
    fn direct_tool_lists_exact_readable_resource_manifest() {
        let (_temp, catalog) = catalog_with_skill();

        let listed = list_resources_tool(
            &catalog,
            &json!({"name": "example", "cursor": 0, "limit": 10}),
        )
        .expect("resource manifest");

        assert_eq!(listed["skill"], "example");
        assert_eq!(listed["totalResources"], 1);
        assert_eq!(listed["resources"][0]["path"], "references/INFO.md");
        assert_eq!(listed["resources"][0]["readable"], true);
        assert_eq!(
            listed["resources"][0]["readArgs"],
            json!({"name": "example", "path": "references/INFO.md"})
        );
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
        assert_eq!(result["_meta"]["truncated"], false);
    }

    #[test]
    fn resource_read_pages_large_skill_markdown_with_a_next_uri() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("skills/large");
        fs::create_dir_all(&skill_dir).expect("skill dir");
        let body = (1..=700)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: large\ndescription: Large skill.\n---\n{body}"),
        )
        .expect("skill");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let first = resource_read(
            &catalog,
            &json!({"uri": "skill://large/SKILL.md?start_line=1&max_bytes=512"}),
        )
        .expect("first page");
        assert_eq!(first["_meta"]["truncated"], true);
        let next_uri = first["_meta"]["nextUri"].as_str().expect("next URI");
        let second = resource_read(&catalog, &json!({"uri": next_uri})).expect("second page");
        assert!(second["_meta"]["startLine"].as_u64().unwrap() > 1);
        assert_ne!(first["contents"][0]["text"], second["contents"][0]["text"]);
    }

    #[test]
    fn canonical_skill_resource_read_returns_full_content_for_native_import() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("skills/native-large");
        fs::create_dir_all(&skill_dir).expect("skill dir");
        let body = "native-import-line\n".repeat(4_000);
        let raw = format!(
            "---\nname: native-large\ndescription: Native import large skill.\n---\n{body}"
        );
        assert!(raw.len() > DEFAULT_SKILL_PAGE_BYTES as usize);
        assert!(raw.len() < catalog::MAX_SKILL_MD_BYTES as usize);
        fs::write(skill_dir.join("SKILL.md"), &raw).expect("skill");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let result = resource_read(&catalog, &json!({"uri": "skill://native-large/SKILL.md"}))
            .expect("native import read");

        assert_eq!(result["contents"][0]["text"], raw);
        assert_eq!(result["_meta"]["truncated"], false);
        assert!(result["_meta"].get("nextUri").is_none());
    }

    #[test]
    fn resource_read_rejects_unbounded_or_unknown_skill_queries() {
        let (_temp, catalog) = catalog_with_skill();
        let too_large = resource_read(
            &catalog,
            &json!({"uri": "skill://example/SKILL.md?max_bytes=131073"}),
        )
        .expect_err("bounded max bytes");
        assert_eq!(too_large["code"], -32602);

        let unknown = resource_read(
            &catalog,
            &json!({"uri": "skill://example/SKILL.md?offset=1"}),
        )
        .expect_err("unknown query");
        assert_eq!(unknown["code"], -32602);
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
