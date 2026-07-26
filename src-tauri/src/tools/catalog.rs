use std::collections::HashSet;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::tools::context::ToolContext;
use crate::tools::workspace::WorkspaceError;

const MAX_EFFECTIVE_TOOLS: usize = 1_024;
const MAX_EFFECTIVE_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_EFFECTIVE_TOOL_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct EffectiveCatalog {
    pub tools: Vec<Value>,
    pub digest: String,
    pub local_count: usize,
    pub proxy_count: usize,
    pub total_bytes: usize,
}

pub fn build_effective_catalog(ctx: &ToolContext) -> Result<EffectiveCatalog, WorkspaceError> {
    build_effective_catalog_from_parts(
        &ctx.tool_profile,
        ctx.skills.is_enabled(),
        ctx.mcp_proxies.list_tools(),
    )
}

pub fn build_effective_catalog_from_parts(
    tool_profile: &str,
    skill_service_enabled: bool,
    proxy_tools: Vec<Value>,
) -> Result<EffectiveCatalog, WorkspaceError> {
    let mut tools = crate::tools::registry::list_tools_for_profile(tool_profile);
    if !skill_service_enabled {
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_none_or(|name| !crate::skills::is_skill_tool(name))
        });
    }
    let local_count = tools.len();
    let proxy_count = proxy_tools.len();
    tools.extend(proxy_tools);

    if tools.len() > MAX_EFFECTIVE_TOOLS {
        return Err(catalog_error(
            "EFFECTIVE_CATALOG_TOO_LARGE",
            format!(
                "Effective MCP catalog contains {} tools; maximum is {MAX_EFFECTIVE_TOOLS}",
                tools.len()
            ),
            json!({"tool_count": tools.len(), "maximum": MAX_EFFECTIVE_TOOLS}),
        ));
    }

    let mut names = HashSet::with_capacity(tools.len());
    let mut total_bytes = 0usize;
    for (index, tool) in tools.iter().enumerate() {
        validate_tool_definition(tool, index)?;
        let name = tool["name"].as_str().expect("validated tool name");
        if !names.insert(name.to_string()) {
            return Err(catalog_error(
                "EFFECTIVE_CATALOG_DUPLICATE_TOOL",
                format!("Effective MCP catalog contains duplicate tool name: {name}"),
                json!({"tool": name}),
            ));
        }
        let bytes = serde_json::to_vec(tool).map_err(|error| {
            catalog_error(
                "EFFECTIVE_CATALOG_SERIALIZATION_FAILED",
                error.to_string(),
                json!({"tool": name}),
            )
        })?;
        if bytes.len() > MAX_EFFECTIVE_TOOL_BYTES {
            return Err(catalog_error(
                "EFFECTIVE_CATALOG_TOOL_TOO_LARGE",
                format!("Tool definition is too large: {name}"),
                json!({
                    "tool": name,
                    "bytes": bytes.len(),
                    "maximum": MAX_EFFECTIVE_TOOL_BYTES
                }),
            ));
        }
        total_bytes = total_bytes.saturating_add(bytes.len());
    }
    if total_bytes > MAX_EFFECTIVE_CATALOG_BYTES {
        return Err(catalog_error(
            "EFFECTIVE_CATALOG_BYTES_EXCEEDED",
            "Effective MCP catalog exceeds the byte budget",
            json!({
                "bytes": total_bytes,
                "maximum": MAX_EFFECTIVE_CATALOG_BYTES
            }),
        ));
    }

    tools.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .cmp(&right.get("name").and_then(Value::as_str))
    });
    let digest = digest_tools(&tools)?;
    Ok(EffectiveCatalog {
        tools,
        digest,
        local_count,
        proxy_count,
        total_bytes,
    })
}

pub fn digest_tools(tools: &[Value]) -> Result<String, WorkspaceError> {
    let canonical = canonicalize(&Value::Array(tools.to_vec()));
    let encoded = serde_json::to_vec(&canonical).map_err(|error| {
        catalog_error(
            "EFFECTIVE_CATALOG_SERIALIZATION_FAILED",
            error.to_string(),
            json!({}),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub fn snapshot_document() -> Result<Value, WorkspaceError> {
    let mut snapshots = Map::new();
    for profile in ["core", "read-only", "advanced"] {
        let catalog = build_effective_catalog_from_parts(profile, true, Vec::new())?;
        snapshots.insert(
            profile.to_string(),
            json!({
                "tool_count": catalog.tools.len(),
                "digest": catalog.digest,
                "names": catalog.tools.iter().filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>()
            }),
        );
    }
    Ok(Value::Object(snapshots))
}

fn validate_tool_definition(tool: &Value, index: usize) -> Result<(), WorkspaceError> {
    let object = tool.as_object().ok_or_else(|| {
        catalog_error(
            "EFFECTIVE_CATALOG_INVALID_TOOL",
            format!("Tool #{index} is not an object"),
            json!({"index": index}),
        )
    })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| valid_tool_name(name))
        .ok_or_else(|| {
            catalog_error(
                "EFFECTIVE_CATALOG_INVALID_TOOL_NAME",
                format!("Tool #{index} has an invalid name"),
                json!({"index": index}),
            )
        })?;
    for field in ["inputSchema", "outputSchema"] {
        let schema = object.get(field).ok_or_else(|| {
            catalog_error(
                "EFFECTIVE_CATALOG_MISSING_SCHEMA",
                format!("Tool {name} is missing {field}"),
                json!({"tool": name, "field": field}),
            )
        })?;
        if !schema.is_object() || schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(catalog_error(
                "EFFECTIVE_CATALOG_INVALID_SCHEMA",
                format!("Tool {name} {field} must be an object-root JSON Schema"),
                json!({"tool": name, "field": field}),
            ));
        }
        if contains_external_ref(schema) {
            return Err(catalog_error(
                "EFFECTIVE_CATALOG_EXTERNAL_SCHEMA_REF",
                format!("Tool {name} {field} contains an external schema reference"),
                json!({"tool": name, "field": field}),
            ));
        }
        jsonschema::meta::validate(schema).map_err(|error| {
            catalog_error(
                "EFFECTIVE_CATALOG_INVALID_SCHEMA",
                format!("Tool {name} {field} is invalid: {error}"),
                json!({"tool": name, "field": field}),
            )
        })?;
    }
    Ok(())
}

fn valid_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn contains_external_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            if matches!(key.as_str(), "$ref" | "$dynamicRef") {
                return value
                    .as_str()
                    .is_some_and(|reference| !reference.starts_with('#'));
            }
            contains_external_ref(value)
        }),
        Value::Array(items) => items.iter().any(contains_external_ref),
        _ => false,
    }
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        _ => value.clone(),
    }
}

fn catalog_error(code: &'static str, message: impl Into<String>, details: Value) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category: "internal",
        retryable: false,
        details,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;
    use serde_json::json;

    use super::{build_effective_catalog_from_parts, snapshot_document};

    fn proxy_tool(name: &str) -> serde_json::Value {
        json!({
            "name": name,
            "title": name,
            "description": "fuzz proxy tool",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": {"ok": {"type": "boolean"}},
                "required": ["ok"],
                "additionalProperties": true
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true
            }
        })
    }

    #[test]
    fn effective_catalog_snapshot_matches_committed_file() {
        let actual = snapshot_document().expect("snapshot");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots/effective_catalog.json");
        if std::env::var_os("UPDATE_EFFECTIVE_CATALOG_SNAPSHOT").is_some() {
            std::fs::create_dir_all(path.parent().expect("snapshot parent")).expect("mkdir");
            std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n")
                .expect("write snapshot");
        }
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("effective catalog snapshot file"),
        )
        .expect("snapshot json");
        assert_eq!(
            actual, expected,
            "run with UPDATE_EFFECTIVE_CATALOG_SNAPSHOT=1"
        );
    }

    #[test]
    fn duplicate_local_or_proxy_name_is_rejected() {
        let error = build_effective_catalog_from_parts("core", true, vec![proxy_tool("read_file")])
            .expect_err("duplicate tool");
        assert_eq!(
            error.to_error_value()["code"],
            "EFFECTIVE_CATALOG_DUPLICATE_TOOL"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn fuzz_catalog_digest_is_stable_across_proxy_order(
            generated in prop::collection::btree_set("[a-z][a-z0-9_]{0,15}", 0..64)
        ) {
            let names = generated.into_iter().collect::<BTreeSet<_>>();
            let first = names.iter().map(|name| proxy_tool(&format!("fuzz__{name}"))).collect::<Vec<_>>();
            let mut second = first.clone();
            second.reverse();
            let first = build_effective_catalog_from_parts("core", true, first).unwrap();
            let second = build_effective_catalog_from_parts("core", true, second).unwrap();
            prop_assert_eq!(first.digest, second.digest);
            prop_assert_eq!(first.tools, second.tools);
        }

        #[test]
        fn fuzz_duplicate_proxy_names_are_always_rejected(name in "[a-z][a-z0-9_]{0,20}") {
            let name = format!("fuzz__{name}");
            let result = build_effective_catalog_from_parts(
                "read-only",
                true,
                vec![proxy_tool(&name), proxy_tool(&name)],
            );
            prop_assert!(result.is_err());
        }
    }
}
