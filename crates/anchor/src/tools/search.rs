use serde_json::{json, Value};

use crate::tools::file;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};
use crate::tools::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Text,
    Symbol,
    Callers,
    Callees,
    Impact,
    Explore,
}

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Symbol => "symbol",
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Impact => "impact",
            Self::Explore => "explore",
        }
    }

    fn is_semantic(self) -> bool {
        !matches!(self, Self::Text)
    }
}

struct SemanticSearchResult {
    engine: &'static str,
    data: Value,
    warnings: Vec<String>,
}

/// Unified code-search entry point. Concrete engines remain an implementation
/// detail so the public contract can evolve independently from ripgrep or
/// CodeGraph process semantics.
pub fn search(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("query is required"))?;
    let requested_mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let mode = resolve_mode(requested_mode, query, args)?;

    if mode.is_semantic() {
        if let Some(result) = semantic_search(ws, query, mode, args, cancellation)? {
            return Ok(tool_ok(json!({
                "query": query,
                "requested_mode": requested_mode,
                "mode": mode.as_str(),
                "engine": result.engine,
                "degraded": false,
                "degraded_reason": null,
                "data": result.data,
                "warnings": result.warnings
            })));
        }
    }

    text_search(
        ws,
        query,
        requested_mode,
        mode,
        args,
        cancellation,
        mode.is_semantic()
            .then_some("semantic backend unavailable; fell back to text search"),
    )
}

fn resolve_mode(
    requested_mode: &str,
    query: &str,
    args: &Value,
) -> Result<SearchMode, WorkspaceError> {
    let explicit = match requested_mode {
        "auto" => None,
        "text" => Some(SearchMode::Text),
        "symbol" => Some(SearchMode::Symbol),
        "callers" => Some(SearchMode::Callers),
        "callees" => Some(SearchMode::Callees),
        "impact" => Some(SearchMode::Impact),
        "explore" => Some(SearchMode::Explore),
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "mode must be auto, text, symbol, callers, callees, impact, or explore",
            ))
        }
    };
    if let Some(mode) = explicit {
        return Ok(mode);
    }

    if text_specific_arguments_present(args) {
        return Ok(SearchMode::Text);
    }
    if query.chars().any(char::is_whitespace) {
        return Ok(SearchMode::Explore);
    }
    if looks_like_symbol(query) {
        return Ok(SearchMode::Symbol);
    }
    Ok(SearchMode::Text)
}

fn text_specific_arguments_present(args: &Value) -> bool {
    args.get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path != ".")
        || args.get("regex").and_then(Value::as_bool) == Some(true)
        || args
            .get("include_globs")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        || args
            .get("exclude_globs")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
        || args.get("context_lines").and_then(Value::as_u64).unwrap_or(0) > 0
        || args.get("cursor").and_then(Value::as_u64).unwrap_or(0) > 0
        || args
            .get("output_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode != "matches")
}

fn looks_like_symbol(query: &str) -> bool {
    !query.is_empty()
        && query.len() <= 256
        && query
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.' | '#' | '$'))
}

fn text_search(
    ws: &Workspace,
    query: &str,
    requested_mode: &str,
    resolved_mode: SearchMode,
    args: &Value,
    cancellation: &CancellationToken,
    degraded_reason: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let mut data = file::grep(ws, args, cancellation)?;
    if let Some(object) = data.as_object_mut() {
        object.remove("ok");
    }
    let engine = data
        .pointer("/scan/engine")
        .and_then(Value::as_str)
        .unwrap_or("anchor");
    let mut warnings = Vec::new();
    if let Some(reason) = degraded_reason {
        warnings.push(reason.to_string());
    }
    Ok(tool_ok(json!({
        "query": query,
        "requested_mode": requested_mode,
        "mode": if degraded_reason.is_some() { "text" } else { resolved_mode.as_str() },
        "engine": engine,
        "degraded": degraded_reason.is_some(),
        "degraded_reason": degraded_reason,
        "data": data,
        "warnings": warnings
    })))
}

/// Semantic search is deliberately an internal backend hook. S3 wires the
/// CodeGraph lifecycle into this function; returning `None` is a supported
/// capability state and deterministically degrades to text search.
fn semantic_search(
    _ws: &Workspace,
    _query: &str,
    _mode: SearchMode,
    _args: &Value,
    _cancellation: &CancellationToken,
) -> Result<Option<SemanticSearchResult>, WorkspaceError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auto_mode_prefers_text_for_explicit_text_controls() {
        let args = json!({"query": "Handler", "regex": true});
        assert_eq!(
            resolve_mode("auto", "Handler", &args).expect("mode"),
            SearchMode::Text
        );
    }

    #[test]
    fn auto_mode_preserves_scoped_path_as_text_search() {
        let args = json!({"query": "Handler", "path": "src/tools"});
        assert_eq!(
            resolve_mode("auto", "Handler", &args).expect("mode"),
            SearchMode::Text
        );
    }

    #[test]
    fn auto_mode_routes_identifier_to_symbol() {
        let args = json!({"query": "dispatch::call_tool"});
        assert_eq!(
            resolve_mode("auto", "dispatch::call_tool", &args).expect("mode"),
            SearchMode::Symbol
        );
    }

    #[test]
    fn auto_mode_routes_natural_language_to_explore() {
        let args = json!({"query": "how does tool dispatch work"});
        assert_eq!(
            resolve_mode("auto", "how does tool dispatch work", &args).expect("mode"),
            SearchMode::Explore
        );
    }

    #[test]
    fn semantic_mode_degrades_to_structured_text_search_when_backend_is_unavailable() {
        let root = tempdir().expect("workspace");
        std::fs::write(
            root.path().join("lib.rs"),
            "fn dispatch() { println!(\"dispatch\"); }\n",
        )
        .expect("source");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");

        let output = search(
            &workspace,
            &json!({"query": "dispatch", "mode": "callers"}),
            &CancellationToken::default(),
        )
        .expect("search");

        assert_eq!(output["ok"], true);
        assert_eq!(output["requested_mode"], "callers");
        assert_eq!(output["mode"], "text");
        assert_eq!(output["degraded"], true);
        assert_eq!(output["data"]["total_matches"], 1);
        assert!(output["degraded_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("semantic backend unavailable")));
    }
}
