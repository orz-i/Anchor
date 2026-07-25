use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillFileSummary {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub mime_type: String,
    pub readable: bool,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillToolResolution {
    pub declared: String,
    pub status: String,
    pub resolved: Option<String>,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: Value,
    pub allowed_tools: Vec<String>,
    pub resolved_tools: Vec<String>,
    pub missing_tools: Vec<String>,
    pub ambiguous_tools: Vec<String>,
    pub tool_resolution: Vec<SkillToolResolution>,
    pub tool_dependencies_evaluated: bool,
    pub tool_compatible: bool,
    pub tool_enforcement_mode: String,
    pub tool_grants_permissions: bool,
    pub source: String,
    pub source_id: String,
    pub relative_path: String,
    pub uri: String,
    pub digest: String,
    pub resources: Vec<SkillFileSummary>,
    pub scripts: Vec<SkillFileSummary>,
    pub script_execution_enabled: bool,
    pub script_execution_policy: String,
    pub resource_truncated: bool,
    pub warnings: Vec<String>,
}

impl SkillSummary {
    pub fn resolve_tools(&mut self, available_tools: &[String]) {
        let available = available_tools.iter().cloned().collect::<HashSet<_>>();
        let mut resolutions = Vec::new();
        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        let mut ambiguous = Vec::new();

        for declared in &self.allowed_tools {
            let canonical = crate::tools::registry::canonical_tool_name(declared);
            let exact = [declared.as_str(), canonical]
                .into_iter()
                .find(|candidate| available.contains(*candidate));
            if let Some(exact) = exact {
                resolved.push(exact.to_string());
                resolutions.push(SkillToolResolution {
                    declared: declared.clone(),
                    status: "resolved".into(),
                    resolved: Some(exact.to_string()),
                    candidates: vec![exact.to_string()],
                });
                continue;
            }

            let suffix = format!("__{declared}");
            let mut candidates = available
                .iter()
                .filter(|name| name.ends_with(&suffix))
                .cloned()
                .collect::<Vec<_>>();
            candidates.sort();
            match candidates.as_slice() {
                [only] => {
                    resolved.push(only.clone());
                    resolutions.push(SkillToolResolution {
                        declared: declared.clone(),
                        status: "resolved".into(),
                        resolved: Some(only.clone()),
                        candidates,
                    });
                }
                [] => {
                    missing.push(declared.clone());
                    resolutions.push(SkillToolResolution {
                        declared: declared.clone(),
                        status: "missing".into(),
                        resolved: None,
                        candidates,
                    });
                }
                _ => {
                    ambiguous.push(declared.clone());
                    resolutions.push(SkillToolResolution {
                        declared: declared.clone(),
                        status: "ambiguous".into(),
                        resolved: None,
                        candidates,
                    });
                }
            }
        }

        resolved.sort();
        resolved.dedup();
        self.resolved_tools = resolved;
        self.missing_tools = missing;
        self.ambiguous_tools = ambiguous;
        self.tool_resolution = resolutions;
        self.tool_dependencies_evaluated = true;
        self.tool_compatible = self.missing_tools.is_empty() && self.ambiguous_tools.is_empty();
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLoadResult {
    #[serde(flatten)]
    pub summary: SkillSummary,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillReadResult {
    pub skill: String,
    pub path: String,
    pub uri: String,
    pub mime_type: String,
    pub encoding: String,
    pub content: String,
    pub size_bytes: u64,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    #[serde(rename = "allowed-tools", default)]
    allowed_tools: String,
}

#[derive(Debug)]
pub struct ParsedSkillMarkdown {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: Value,
    pub allowed_tools: Vec<String>,
    pub instructions: String,
}

pub fn parse_skill_markdown(
    raw: &str,
    directory_name: &str,
) -> Result<ParsedSkillMarkdown, String> {
    let (frontmatter, instructions) = split_frontmatter(raw)?;
    let parsed: RawSkillFrontmatter = serde_yaml::from_str(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter 无效：{error}"))?;
    validate_name(&parsed.name, directory_name)?;

    let description = parsed.description.trim().to_string();
    if description.is_empty() {
        return Err("SKILL.md description 不能为空".into());
    }
    if description.chars().count() > 1024 {
        return Err("SKILL.md description 不能超过 1024 个字符".into());
    }
    if parsed
        .compatibility
        .as_deref()
        .is_some_and(|value| value.chars().count() > 500)
    {
        return Err("SKILL.md compatibility 不能超过 500 个字符".into());
    }
    if instructions.lines().count() > 500 || instructions.chars().count() > 20_000 {
        return Err("SKILL.md instructions 必须控制在 500 行且约 20,000 字符以内".into());
    }

    let mut allowed_tools = Vec::new();
    for value in parsed.allowed_tools.split_whitespace() {
        if value.len() > 128
            || !value.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(
                        character,
                        '_' | '-' | '.' | ':' | '/' | '*' | '?' | '(' | ')'
                    )
            })
        {
            return Err(format!(
                "SKILL.md allowed-tools 包含无效工具选择器：{value}"
            ));
        }
        if !allowed_tools.iter().any(|existing| existing == value) {
            allowed_tools.push(value.to_string());
        }
    }

    let metadata = serde_json::to_value(parsed.metadata)
        .map_err(|error| format!("SKILL.md metadata 无法序列化：{error}"))?;
    Ok(ParsedSkillMarkdown {
        name: parsed.name,
        description,
        license: parsed.license.filter(|value| !value.trim().is_empty()),
        compatibility: parsed
            .compatibility
            .filter(|value| !value.trim().is_empty()),
        metadata,
        allowed_tools,
        instructions: instructions.trim_start().to_string(),
    })
}

fn split_frontmatter(raw: &str) -> Result<(&str, &str), String> {
    let first_newline = raw
        .find('\n')
        .ok_or_else(|| "SKILL.md 缺少 YAML frontmatter".to_string())?;
    if raw[..first_newline].trim_end_matches('\r') != "---" {
        return Err("SKILL.md 必须以 YAML frontmatter 开始".into());
    }

    let mut offset = first_newline + 1;
    for line in raw[offset..].split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        if line_without_newline == "---" {
            let frontmatter_end = offset;
            let body_start = offset + line.len();
            return Ok((&raw[first_newline + 1..frontmatter_end], &raw[body_start..]));
        }
        offset += line.len();
    }
    Err("SKILL.md frontmatter 缺少结束分隔符 ---".into())
}

fn validate_name(name: &str, directory_name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("Skill name 长度必须为 1-64 个字符".into());
    }
    if name != directory_name {
        return Err(format!(
            "Skill name 必须与目录名一致：frontmatter={name}，目录={directory_name}"
        ));
    }
    let valid = name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid {
        return Err(
            "Skill name 只能包含小写字母、数字和单个连字符，且不能以连字符开头或结尾".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_skill_frontmatter() {
        let parsed = parse_skill_markdown(
            "---\nname: code-review\ndescription: Review code safely.\nallowed-tools: read_file git_diff\nmetadata:\n  version: '1'\n---\n# Instructions\nReview the diff.\n",
            "code-review",
        )
        .expect("parse skill");

        assert_eq!(parsed.name, "code-review");
        assert_eq!(parsed.allowed_tools, vec!["read_file", "git_diff"]);
        assert!(parsed.instructions.contains("Review the diff"));
        assert_eq!(parsed.metadata["version"], "1");
    }

    #[test]
    fn rejects_name_that_does_not_match_directory() {
        let error = parse_skill_markdown(
            "---\nname: other\ndescription: Test skill.\n---\nBody",
            "expected",
        )
        .expect_err("mismatch");

        assert!(error.contains("目录名一致"));
    }

    #[test]
    fn rejects_unknown_frontmatter_and_non_string_metadata() {
        let unknown = parse_skill_markdown(
            "---\nname: example\ndescription: Test.\ncustom-field: true\n---\nBody",
            "example",
        )
        .expect_err("unknown field");
        assert!(unknown.contains("unknown field"));

        let metadata = parse_skill_markdown(
            "---\nname: example\ndescription: Test.\nmetadata:\n  nested:\n    value: true\n---\nBody",
            "example",
        )
        .expect_err("string metadata");
        assert!(metadata.contains("metadata"));
    }

    #[test]
    fn resolves_prefixed_proxy_tools_and_reports_missing_dependencies() {
        let mut summary = SkillSummary {
            name: "example".into(),
            description: "Example".into(),
            license: None,
            compatibility: None,
            metadata: Value::Object(Default::default()),
            allowed_tools: vec!["read_file".into(), "start_feature".into(), "missing".into()],
            resolved_tools: Vec::new(),
            missing_tools: Vec::new(),
            ambiguous_tools: Vec::new(),
            tool_resolution: Vec::new(),
            tool_dependencies_evaluated: false,
            tool_compatible: true,
            tool_enforcement_mode: "declarative-only".into(),
            tool_grants_permissions: false,
            source: "workspace".into(),
            source_id: "workspace-agents".into(),
            relative_path: "example".into(),
            uri: "skill://example/SKILL.md".into(),
            digest: "sha256:test".into(),
            resources: Vec::new(),
            scripts: Vec::new(),
            script_execution_enabled: false,
            script_execution_policy: "confirm-via-exec-command".into(),
            resource_truncated: false,
            warnings: Vec::new(),
        };
        summary.resolve_tools(&["read_file".into(), "mcp_probe_kit__start_feature".into()]);
        assert_eq!(summary.resolved_tools.len(), 2);
        assert_eq!(summary.missing_tools, vec!["missing"]);
        assert!(!summary.tool_compatible);
    }
}
