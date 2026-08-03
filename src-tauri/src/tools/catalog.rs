use std::collections::HashSet;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::tools::context::ToolContext;
use crate::tools::workspace::WorkspaceError;

pub const MAX_CHATGPT_CATALOG_TOOLS: usize = 128;
pub const MAX_CHATGPT_CATALOG_BYTES: usize = 512 * 1024;
pub const MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS: usize = 96 * 1024;
const MAX_EFFECTIVE_TOOL_BYTES: usize = 128 * 1024;
const ESTIMATED_BYTES_PER_TOKEN: usize = 4;

#[derive(Debug, Clone)]
pub struct EffectiveCatalog {
    pub tools: Vec<Value>,
    pub digest: String,
    pub local_count: usize,
    pub proxy_count: usize,
    pub total_bytes: usize,
    pub estimated_tokens: usize,
}

impl EffectiveCatalog {
    pub fn metrics_value(&self) -> Value {
        json!({
            "local_tool_count": self.local_count,
            "proxy_tool_count": self.proxy_count,
            "tool_count": self.tools.len(),
            "catalog_bytes": self.total_bytes,
            "estimated_tokens": self.estimated_tokens,
            "budget": catalog_budget_value()
        })
    }
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
    mut proxy_tools: Vec<Value>,
) -> Result<EffectiveCatalog, WorkspaceError> {
    let mut tools = crate::tools::registry::list_tools_for_profile(tool_profile);
    if !skill_service_enabled {
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_none_or(|name| !crate::skills::is_skill_tool(name))
        });
    }
    tools.sort_by(tool_name_order);
    proxy_tools.sort_by(tool_name_order);
    let local_count = tools.len();
    let proxy_count = proxy_tools.len();
    tools.extend(proxy_tools);

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
    let estimated_tokens = estimate_catalog_tokens(total_bytes);
    enforce_chatgpt_catalog_budget(
        local_count,
        proxy_count,
        tools.len(),
        total_bytes,
        estimated_tokens,
    )?;

    let digest = digest_tools(&tools)?;
    Ok(EffectiveCatalog {
        tools,
        digest,
        local_count,
        proxy_count,
        total_bytes,
        estimated_tokens,
    })
}

fn tool_name_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    left.get("name")
        .and_then(Value::as_str)
        .cmp(&right.get("name").and_then(Value::as_str))
}

pub fn estimate_catalog_tokens(total_bytes: usize) -> usize {
    total_bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN)
}

fn enforce_chatgpt_catalog_budget(
    local_count: usize,
    proxy_count: usize,
    tool_count: usize,
    total_bytes: usize,
    estimated_tokens: usize,
) -> Result<(), WorkspaceError> {
    let exceeded = tool_count > MAX_CHATGPT_CATALOG_TOOLS
        || total_bytes > MAX_CHATGPT_CATALOG_BYTES
        || estimated_tokens > MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS;
    if !exceeded {
        return Ok(());
    }

    Err(catalog_error(
        "EFFECTIVE_CATALOG_CHATGPT_BUDGET_EXCEEDED",
        format!(
            "Anchor MCP tool catalog exceeds the ChatGPT compatibility budget: {tool_count} tools, {total_bytes} bytes, approximately {estimated_tokens} tokens"
        ),
        json!({
            "reason": "chatgpt_catalog_budget_exceeded",
            "local_tool_count": local_count,
            "proxy_tool_count": proxy_count,
            "tool_count": tool_count,
            "catalog_bytes": total_bytes,
            "estimated_tokens": estimated_tokens,
            "budget": catalog_budget_value(),
            "suggestions": [
                "Set includeTools, excludeTools, or maxTools on downstream MCP servers",
                "Use the core or read-only Anchor tool profile",
                "Restart Anchor and refresh or recreate the ChatGPT app after reducing the catalog"
            ]
        }),
    ))
}

fn catalog_budget_value() -> Value {
    json!({
        "max_tools": MAX_CHATGPT_CATALOG_TOOLS,
        "max_catalog_bytes": MAX_CHATGPT_CATALOG_BYTES,
        "max_estimated_tokens": MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS,
        "estimated_bytes_per_token": ESTIMATED_BYTES_PER_TOKEN
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
                "catalog_version": crate::tools::registry::CATALOG_VERSION,
                "tool_count": catalog.tools.len(),
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

    use super::{
        build_effective_catalog_from_parts, snapshot_document, MAX_CHATGPT_CATALOG_BYTES,
        MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS, MAX_CHATGPT_CATALOG_TOOLS,
    };

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

    fn browser_tools(count: usize) -> Vec<serde_json::Value> {
        (0..count)
            .map(|index| proxy_tool(&format!("browser__action_{index:02}")))
            .collect()
    }

    #[test]
    fn advanced_plus_browser_catalog_stays_within_chatgpt_budget() {
        let catalog = build_effective_catalog_from_parts("advanced", true, browser_tools(48))
            .expect("advanced plus browser catalog");

        assert_eq!(catalog.local_count, 61);
        assert_eq!(catalog.proxy_count, 48);
        assert!(catalog.tools.len() <= MAX_CHATGPT_CATALOG_TOOLS);
        assert!(catalog.total_bytes <= MAX_CHATGPT_CATALOG_BYTES);
        assert!(catalog.estimated_tokens <= MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS);
    }

    #[test]
    fn core_plus_browser_catalog_stays_within_chatgpt_budget() {
        let catalog = build_effective_catalog_from_parts("core", true, browser_tools(48))
            .expect("core plus browser catalog");

        assert_eq!(catalog.local_count, 43);
        assert_eq!(catalog.proxy_count, 48);
        assert!(catalog.tools[..catalog.local_count].iter().all(|tool| {
            !tool["name"]
                .as_str()
                .unwrap_or_default()
                .starts_with("browser__action_")
        }));
        assert!(catalog.tools[catalog.local_count..].iter().all(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_default()
                .starts_with("browser__action_")
        }));
        assert!(catalog.tools.len() <= MAX_CHATGPT_CATALOG_TOOLS);
        assert!(catalog.total_bytes <= MAX_CHATGPT_CATALOG_BYTES);
        assert!(catalog.estimated_tokens <= MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS);
    }

    #[test]
    fn restricted_browser_catalog_stays_within_chatgpt_budget() {
        let catalog = build_effective_catalog_from_parts("core", true, browser_tools(8))
            .expect("restricted browser catalog");

        assert_eq!(catalog.local_count, 43);
        assert_eq!(catalog.proxy_count, 8);
        assert_eq!(catalog.tools.len(), 51);
        assert!(catalog.total_bytes <= MAX_CHATGPT_CATALOG_BYTES);
        assert!(catalog.estimated_tokens <= MAX_CHATGPT_CATALOG_ESTIMATED_TOKENS);
    }

    #[test]
    fn over_budget_catalog_returns_actionable_diagnostics() {
        let error = build_effective_catalog_from_parts("advanced", true, browser_tools(100))
            .expect_err("catalog should exceed the tool-count budget");
        let diagnostic = error.to_error_value();

        assert_eq!(
            diagnostic["code"],
            "EFFECTIVE_CATALOG_CHATGPT_BUDGET_EXCEEDED"
        );
        assert_eq!(
            diagnostic["details"]["reason"],
            "chatgpt_catalog_budget_exceeded"
        );
        assert_eq!(diagnostic["details"]["local_tool_count"], 61);
        assert_eq!(diagnostic["details"]["proxy_tool_count"], 100);
        assert!(diagnostic["details"]["suggestions"]
            .as_array()
            .is_some_and(|suggestions| suggestions.iter().any(|suggestion| suggestion
                .as_str()
                .is_some_and(|text| text.contains("includeTools")))));
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
