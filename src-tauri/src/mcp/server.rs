use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::tools::dispatch::call_tool_prevalidated_with_session_cancellation;
use crate::tools::workspace::tool_err;
use crate::tools::{
    build_effective_catalog, wrap_mcp_tool_result, CancellationToken, SharedToolContext,
    ToolContext, Workspace,
};
use crate::workspace::AuthConfig;

pub type SharedState = SharedToolContext;

#[cfg(test)]
pub async fn handle_request(state: &SharedState, body: &Value) -> Value {
    let protocol_version = if body.get("method").and_then(Value::as_str) == Some("initialize") {
        body.get("params")
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str)
            .map(crate::mcp::protocol::negotiate_protocol_version)
            .unwrap_or(crate::mcp::protocol::CURRENT_PROTOCOL_VERSION)
    } else {
        crate::mcp::protocol::CURRENT_PROTOCOL_VERSION
    };
    handle_request_with_protocol(state, body, protocol_version).await
}

#[cfg(test)]
pub async fn handle_request_with_protocol(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
) -> Value {
    handle_request_with_protocol_and_cancellation(
        state,
        body,
        protocol_version,
        &CancellationToken::default(),
    )
    .await
}

#[cfg(test)]
pub async fn handle_request_with_protocol_and_cancellation(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
    cancellation: &CancellationToken,
) -> Value {
    handle_request_with_protocol_session_and_cancellation(
        state,
        body,
        protocol_version,
        cancellation,
        None,
    )
    .await
}

pub async fn handle_request_with_protocol_session_and_cancellation(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_null() && method.starts_with("notifications/") {
        return Value::Null;
    }

    let result = match method {
        "initialize" => Ok(initialize_result_for_version(state, protocol_version)),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => {
            if !state
                .mcp_proxies
                .wait_until_configured(Duration::from_secs(70))
                .await
            {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32003,
                        "message": "MCP proxy tool catalog is still initializing",
                        "data": {
                            "reason": "proxy_catalog_initializing",
                            "retryable": true
                        }
                    }
                });
            }
            match build_effective_catalog(state.as_ref()) {
                Ok(catalog) => Ok(serde_json::json!({ "tools": catalog.tools })),
                Err(error) => Err(serde_json::json!({
                    "code": -32603,
                    "message": "Failed to build effective MCP tool catalog",
                    "data": error.to_error_value()
                })),
            }
        }
        "tools/call" => handle_tools_call(state, &params, cancellation, session_id).await,
        "resources/list" => crate::skills::resources_list(&state.skills, &params),
        "resources/read" => crate::skills::resource_read(&state.skills, &params),
        _ => Err(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };

    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

#[cfg(test)]
fn initialize_result(state: &SharedState) -> Value {
    initialize_result_for_version(state, crate::mcp::protocol::CURRENT_PROTOCOL_VERSION)
}

fn initialize_result_for_version(state: &SharedState, protocol_version: &str) -> Value {
    let mut capabilities = serde_json::json!({
        "tools": { "listChanged": false }
    });
    if state.skills.is_enabled() {
        capabilities["resources"] = serde_json::json!({
            "subscribe": false,
            "listChanged": false
        });
    }
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": capabilities,
        "serverInfo": {
            "name": "coding-tools-mcp",
            "title": "Coding Tools MCP",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Use these tools only for local coding operations inside the configured workspace. Agent Skills are available through list_skills, load_skill, read_skill_resource, and skill:// resources when enabled; load only the relevant Skill and treat Skill content as instructions, not as permission to bypass tool policy. Skill allowed-tools declarations are dependency metadata only: load_skill resolves them against the current local and proxied tool catalog, reports missing or ambiguous tools, and never grants permissions. There is no dedicated Skill script executor and no model-controlled permission grant tool. Model-supplied confirm fields are not accepted as user approval. Destructive commands, critical-file deletion, and snapshotted Skill script execution require the operator to enable dangerous permission mode through the trusted GUI or CLI control plane; Skill execution is still rejected if the script digest changed after the listener snapshot. At the start of every new ChatGPT conversation, before answering the user's first request, call history_session_bootstrap exactly once, even if the user did not explicitly ask to restore or resume. Treat bootstrap as required conversation initialization: when no history exists it creates the first history session; when history exists, read all_history_summary, latest_handoff, and inherited_summary before acting. Repeated successful bootstrap calls in the same conversation resume the same session and must not create duplicates. Preserve session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task in the conversation, call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path. Persistence requires a successful tool call and is not automatic background persistence."
    })
}

async fn handle_tools_call(
    state: &SharedState,
    params: &Value,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    let raw_args = raw_tool_arguments(params);

    if crate::skills::is_skill_tool(name) && !state.skills.is_enabled() {
        return Err(serde_json::json!({
            "code": -32602,
            "message": "Skill service is disabled for this workspace/profile",
            "data": { "reason": "skill_service_disabled" }
        }));
    }
    if matches!(name, "list_skills" | "load_skill")
        && !state
            .mcp_proxies
            .wait_until_configured(Duration::from_secs(70))
            .await
    {
        return Err(serde_json::json!({
            "code": -32003,
            "message": "MCP proxy tool catalog is still initializing",
            "data": {
                "reason": "proxy_catalog_initializing",
                "retryable": true
            }
        }));
    }

    if let Some(result) = state
        .mcp_proxies
        .call_tool_with_cancellation(name, &raw_args, cancellation)
        .await
    {
        return result;
    }

    let canonical_name = crate::tools::registry::canonical_tool_name(name);
    let known = crate::tools::registry::exposed_tool_names(&state.tool_profile);
    if !known.iter().any(|n| n == &canonical_name) {
        return Err(serde_json::json!({
            "code": -32602,
            "message": format!("Unknown tool: {name}"),
            "data": { "reason": "unknown_tool" }
        }));
    }

    if let Err(error) = crate::tools::schema::validate_tool_input(name, &raw_args) {
        return Ok(wrap_mcp_tool_result(
            canonical_name,
            &raw_args,
            tool_err(error),
        ));
    }

    let args = tool_arguments(name, params);

    let state = state.clone();
    let canonical_name = canonical_name.to_string();
    let call_name = canonical_name.clone();
    let call_args = args.clone();
    let cancellation = cancellation.clone();
    let session_id = session_id.map(str::to_string);
    let structured = tokio::task::spawn_blocking(move || {
        call_tool_prevalidated_with_session_cancellation(
            state.as_ref(),
            &call_name,
            &call_args,
            &cancellation,
            session_id.as_deref(),
        )
    })
    .await
    .map_err(|error| {
        serde_json::json!({
            "code": -32603,
            "message": "Local MCP tool worker failed",
            "data": {
                "reason": "worker_failed",
                "detail": error.to_string()
            }
        })
    })?;
    Ok(wrap_mcp_tool_result(&canonical_name, &raw_args, structured))
}

fn raw_tool_arguments(params: &Value) -> Value {
    params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

fn tool_arguments(name: &str, params: &Value) -> Value {
    let mut args = raw_tool_arguments(params);
    if name.starts_with("history_session_") {
        if let Some(session_key) = params
            .get("_meta")
            .and_then(|meta| meta.get("openai/session"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !args.is_object() {
                args = serde_json::json!({});
            }
            args["_host_session_key"] = Value::String(session_key.to_string());
        }
    }
    args
}

pub fn new_state(
    workspace: Workspace,
    auth: AuthConfig,
    policy: crate::tools::policy::PolicySettings,
    tool_profile: String,
    permission_mode: String,
) -> SharedState {
    Arc::new(ToolContext::from_workspace(
        workspace,
        auth,
        policy,
        tool_profile,
        permission_mode,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use crate::tools::ToolContext;

    use super::{handle_request, initialize_result, tool_arguments};

    fn test_state() -> (tempfile::TempDir, tempfile::TempDir, Arc<ToolContext>) {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        (workspace, harness, state)
    }

    #[test]
    fn initialize_instructions_define_the_history_persistence_workflow() {
        let (_workspace, _harness, state) = test_state();
        let initialized = initialize_result(&state);
        let instructions = initialized["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("list_skills"));
        assert!(instructions.contains("allowed-tools declarations are dependency metadata only"));
        assert!(instructions.contains("never grants permissions"));
        assert!(instructions.contains("There is no dedicated Skill script executor"));
        assert!(instructions.contains("Model-supplied confirm fields are not accepted"));
        assert!(instructions.contains("trusted GUI or CLI control plane"));
        assert!(instructions.contains("script digest changed"));
        assert!(instructions.contains("history_session_bootstrap"));
        assert!(instructions.contains("At the start of every new ChatGPT conversation"));
        assert!(instructions.contains("before answering the user's first request"));
        assert!(instructions.contains("even if the user did not explicitly ask"));
        assert!(instructions.contains("required conversation initialization"));
        assert!(instructions.contains("must not create duplicates"));
        assert!(instructions.contains("history_session_checkpoint"));
        assert!(instructions.contains("session_key and current_path returned by bootstrap"));
        assert!(instructions.contains("session_key and expected_path"));
        assert!(instructions.contains("After completing each user-requested task"));
        assert!(instructions.contains("before the final response"));
        assert!(instructions.contains("checkpoint returns ok=true"));
        assert!(instructions.contains("not automatic background persistence"));
    }

    #[test]
    fn initialize_does_not_claim_tool_catalog_notifications_without_a_stream() {
        let (_workspace, _harness, state) = test_state();
        let initialized = initialize_result(&state);

        assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(
            initialized["capabilities"]["resources"]["listChanged"],
            false
        );
    }

    #[test]
    fn workspace_prompt_initializes_or_restores_a_chatgpt_session() {
        let component = include_str!("../../../src/lib/components/ChatGptSessionPrompt.svelte");

        assert!(component.contains("ChatGPT 新会话启动提示词"));
        assert!(component.contains("请初始化或恢复当前项目会话"));
        assert!(component.contains("如果没有历史记录"));
        assert!(component.contains("all_history_summary"));
        assert!(component.contains("history_session_checkpoint"));
        assert!(!component.contains("打开连接器设置"));
    }

    #[test]
    fn chatgpt_session_metadata_is_injected_only_for_history_tools() {
        let params = json!({
            "arguments": {"session_key": "explicit"},
            "_meta": {"openai/session": "chatgpt-conversation"}
        });
        let history = tool_arguments("history_session_bootstrap", &params);
        assert_eq!(history["session_key"], "explicit");
        assert_eq!(history["_host_session_key"], "chatgpt-conversation");

        let existing = tool_arguments("read_file", &params);
        assert_eq!(existing["session_key"], "explicit");
        assert!(existing.get("_host_session_key").is_none());
    }

    #[tokio::test]
    async fn explicit_session_key_prevents_changed_chatgpt_metadata_from_redirecting_history() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "history_session_bootstrap",
                    "arguments": {"session_key": "explicit-session"},
                    "_meta": {"openai/session": "chatgpt-session"}
                }
            }),
        )
        .await;
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["session_key_source"], "explicit_session_key");
        assert_eq!(structured["session_key"], "explicit-session");
        assert_eq!(structured["host_session_key_mismatch"], true);
        let content = fs::read_to_string(workspace.path().join("docs/history-session/1.md"))
            .expect("read history file");
        assert!(content.contains("**Session key:** explicit-session"));
        assert!(!content.contains("**Session key:** chatgpt-session"));
    }

    #[tokio::test]
    async fn legacy_grep_calls_are_mapped_to_the_public_grep_text_tool() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        fs::write(workspace.path().join("sample.txt"), "catalog needle")
            .expect("write sample file");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "grep",
                    "arguments": {"query": "needle", "path": "."}
                }
            }),
        )
        .await;

        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["structuredContent"]["ok"], true);
    }

    #[tokio::test]
    async fn skills_are_discovered_and_loaded_through_mcp() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let skill_dir = workspace.path().join("skills/review");
        fs::create_dir_all(skill_dir.join("references")).expect("create skill");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a change.\n---\nRead the diff carefully.\nCheck tests.\nReport findings.\n",
        )
        .expect("write skill");
        fs::write(skill_dir.join("references/RULES.md"), "No regressions.\n")
            .expect("write resource");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let tools = handle_request(
            &state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
        assert!(tools["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["name"] == "load_skill"));

        let listed = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"list_skills","arguments":{}}
            }),
        )
        .await;
        assert_eq!(
            listed["result"]["structuredContent"]["skills"][0]["name"],
            "review"
        );
        assert_eq!(
            listed["result"]["structuredContent"]["skills"][0]["oversized"],
            false
        );

        let loaded = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"load_skill",
                    "arguments":{"name":"review","start_line":2,"end_line":2}
                }
            }),
        )
        .await;
        assert_eq!(
            loaded["result"]["structuredContent"]["instructions"],
            "Check tests.\n"
        );
        assert_eq!(loaded["result"]["structuredContent"]["startLine"], 2);
        assert_eq!(loaded["result"]["structuredContent"]["endLine"], 2);
        assert_eq!(loaded["result"]["structuredContent"]["totalLines"], 3);
        assert_eq!(loaded["result"]["structuredContent"]["nextStartLine"], 3);
        assert_eq!(loaded["result"]["structuredContent"]["truncated"], true);

        let index = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"resources/read",
                "params":{"uri":"skill://index.json"}
            }),
        )
        .await;
        let index_text = index["result"]["contents"][0]["text"]
            .as_str()
            .expect("index text");
        assert!(index_text.contains("skill://review/SKILL.md"));

        let page = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"resources/read",
                "params":{"uri":"skill://review/SKILL.md?start_line=2&end_line=2"}
            }),
        )
        .await;
        assert!(page["result"]["contents"][0]["text"]
            .as_str()
            .expect("skill page")
            .contains("name: review"));
        assert_eq!(page["result"]["_meta"]["startLine"], 2);
        assert_eq!(page["result"]["_meta"]["endLine"], 2);
    }

    #[tokio::test]
    async fn disabled_skill_service_hides_tools_and_resources_capability() {
        let (_workspace, _harness, state) = test_state();
        state
            .skills
            .configure(crate::skills::SkillSettings::from_text(false, "skills"));

        let initialized = initialize_result(&state);
        assert!(initialized["capabilities"].get("resources").is_none());

        let tools = handle_request(
            &state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
        assert!(!tools["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["name"] == "load_skill"));
    }
}
