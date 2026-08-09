use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const RECOMMENDED_INSTRUCTION_LINES: usize = 500;
pub(super) const RECOMMENDED_INSTRUCTION_TOKENS: usize = 5_000;

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
    pub instruction_lines: usize,
    pub instruction_chars: usize,
    pub instruction_bytes: u64,
    pub estimated_tokens: usize,
    pub oversized: bool,
    pub quality_warnings: Vec<String>,
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
            if available.contains(declared) {
                resolved.push(declared.clone());
                resolutions.push(SkillToolResolution {
                    declared: declared.clone(),
                    status: "resolved".into(),
                    resolved: Some(declared.clone()),
                    candidates: vec![declared.clone()],
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
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub total_bytes: u64,
    pub returned_bytes: usize,
    pub truncated: bool,
    pub next_start_line: Option<usize>,
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
    pub returned_bytes: usize,
    pub total_lines: Option<usize>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub truncated: bool,
    pub next_start_line: Option<usize>,
}

#[derive(Debug)]
pub(super) struct TextPage {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub total_bytes: u64,
    pub returned_bytes: usize,
    pub truncated: bool,
    pub next_start_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RawSkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, serde_yaml::Value>,
    #[serde(rename = "allowed-tools", default)]
    allowed_tools: String,
    #[serde(flatten)]
    extensions: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug)]
pub struct ParsedSkillMarkdown {
    pub name: String,
    pub description: String,
    pub frontmatter: Value,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: Value,
    pub allowed_tools: Vec<String>,
    pub instructions: String,
    pub instruction_lines: usize,
    pub instruction_chars: usize,
    pub instruction_bytes: u64,
    pub estimated_tokens: usize,
    pub oversized: bool,
    pub quality_warnings: Vec<String>,
}

pub fn parse_skill_markdown(
    raw: &str,
    directory_name: &str,
) -> Result<ParsedSkillMarkdown, String> {
    let (frontmatter, instructions) = split_frontmatter(raw)?;
    let frontmatter_value = serde_yaml::from_str::<serde_yaml::Value>(frontmatter)
        .map_err(|error| format!("SKILL.md frontmatter 无效：{error}"))?;
    let frontmatter_json = serde_json::to_value(&frontmatter_value)
        .map_err(|error| format!("SKILL.md frontmatter 无法转换为 JSON：{error}"))?;
    if !frontmatter_json.is_object() {
        return Err("SKILL.md frontmatter 必须是对象".into());
    }
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

    let mut metadata = parsed.metadata;
    for (key, value) in parsed.extensions {
        metadata.entry(key).or_insert(value);
    }
    let metadata = serde_json::to_value(metadata)
        .map_err(|error| format!("SKILL.md metadata 无法序列化：{error}"))?;
    let instructions = instructions.trim_start().to_string();
    let instruction_lines = instructions.lines().count();
    let instruction_chars = instructions.chars().count();
    let instruction_bytes = instructions.len() as u64;
    let estimated_tokens = estimate_instruction_tokens(&instructions);
    let mut quality_warnings = Vec::new();
    if instruction_lines > RECOMMENDED_INSTRUCTION_LINES {
        quality_warnings.push(format!(
            "SKILL.md instructions 为 {instruction_lines} 行，超过建议的 {RECOMMENDED_INSTRUCTION_LINES} 行；建议将详细资料拆分到 references/"
        ));
    }
    if estimated_tokens > RECOMMENDED_INSTRUCTION_TOKENS {
        quality_warnings.push(format!(
            "SKILL.md instructions 预计约 {estimated_tokens} tokens，超过建议的 {RECOMMENDED_INSTRUCTION_TOKENS} tokens；建议采用渐进式资源加载"
        ));
    }
    let oversized = !quality_warnings.is_empty();

    Ok(ParsedSkillMarkdown {
        name: parsed.name,
        description,
        frontmatter: frontmatter_json,
        license: parsed.license.filter(|value| !value.trim().is_empty()),
        compatibility: parsed
            .compatibility
            .filter(|value| !value.trim().is_empty()),
        metadata,
        allowed_tools,
        instructions,
        instruction_lines,
        instruction_chars,
        instruction_bytes,
        estimated_tokens,
        oversized,
        quality_warnings,
    })
}

pub(super) fn paginate_text(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: usize,
) -> Result<TextPage, String> {
    let total_bytes = text.len() as u64;
    let lines = if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect::<Vec<_>>()
    };
    let total_lines = lines.len();
    if total_lines == 0 {
        return Ok(TextPage {
            content: String::new(),
            start_line: 1,
            end_line: 0,
            total_lines: 0,
            total_bytes,
            returned_bytes: 0,
            truncated: false,
            next_start_line: None,
        });
    }

    let start = start_line.unwrap_or(1).max(1);
    if start > total_lines {
        return Err(format!(
            "start_line={start} 超过 instructions 总行数 {total_lines}"
        ));
    }
    let requested_end = end_line.unwrap_or(total_lines);
    if requested_end < start {
        return Err("end_line 不能小于 start_line".into());
    }
    let end = requested_end.min(total_lines);
    let mut content = String::new();
    let mut actual_end = start.saturating_sub(1);
    for (index, line) in lines[start - 1..end].iter().enumerate() {
        if content.len().saturating_add(line.len()) > max_bytes {
            if content.is_empty() {
                return Err(format!(
                    "第 {start} 行大小为 {} 字节，超过 max_bytes={max_bytes}；请提高 max_bytes 后重试",
                    line.len()
                ));
            }
            break;
        }
        content.push_str(line);
        actual_end = start + index;
    }
    let returned_bytes = content.len();
    let next_start_line = (actual_end < total_lines).then_some(actual_end + 1);
    Ok(TextPage {
        content,
        start_line: start,
        end_line: actual_end,
        total_lines,
        total_bytes,
        returned_bytes,
        truncated: start > 1 || actual_end < total_lines,
        next_start_line,
    })
}

fn estimate_instruction_tokens(text: &str) -> usize {
    let mut ascii_word_like = 0usize;
    let mut ascii_symbols = 0usize;
    let mut non_ascii = 0usize;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character.is_ascii_whitespace() {
            ascii_word_like += 1;
        } else if character.is_ascii() {
            ascii_symbols += 1;
        } else {
            non_ascii += 1;
        }
    }
    ascii_word_like.div_ceil(4) + ascii_symbols.div_ceil(2) + non_ascii
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
    fn accepts_extension_frontmatter_and_structured_metadata() {
        let extended = parse_skill_markdown(
            "---\nname: example\ndescription: Test.\nrisk: medium\ncategory: architecture\nuser-invocable: true\nmetadata:\n  risk: explicit\n---\nBody",
            "example",
        )
        .expect("extension fields");
        assert_eq!(extended.metadata["risk"], "explicit");
        assert_eq!(extended.metadata["category"], "architecture");
        assert_eq!(extended.metadata["user-invocable"], true);

        let metadata = parse_skill_markdown(
            "---\nname: example\ndescription: Test.\nmetadata:\n  nested:\n    value: true\n  priority: 5\n  tags: [one, two]\n---\nBody",
            "example",
        )
        .expect("structured metadata");
        assert_eq!(metadata.metadata["nested"]["value"], true);
        assert_eq!(metadata.metadata["priority"], 5);
        assert_eq!(metadata.metadata["tags"][1], "two");
    }

    #[test]
    fn long_instructions_are_accepted_with_quality_warnings() {
        let short_lines = (0..600)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_skill_markdown(
            &format!("---\nname: example\ndescription: Test.\n---\n{short_lines}"),
            "example",
        )
        .expect("long line count remains valid");
        assert_eq!(parsed.instruction_lines, 600);
        assert!(parsed.oversized);
        assert!(parsed
            .quality_warnings
            .iter()
            .any(|warning| warning.contains("500")));

        let english = "This is a detailed reusable instruction for the agent. ".repeat(600);
        assert!(english.chars().count() > 20_000);
        let parsed = parse_skill_markdown(
            &format!("---\nname: example\ndescription: Test.\n---\n{english}"),
            "example",
        )
        .expect("instructions above the former character limit remain valid");
        assert!(parsed.instruction_lines < RECOMMENDED_INSTRUCTION_LINES);
        assert!(parsed.estimated_tokens > RECOMMENDED_INSTRUCTION_TOKENS);
        assert!(parsed.oversized);

        let chinese = "这是一个用于验证中文上下文预算的技能指令。".repeat(300);
        let parsed = parse_skill_markdown(
            &format!("---\nname: example\ndescription: Test.\n---\n{chinese}"),
            "example",
        )
        .expect("long Chinese remains valid");
        assert!(parsed.estimated_tokens > RECOMMENDED_INSTRUCTION_TOKENS);
        assert!(parsed.oversized);
    }

    #[test]
    fn text_pagination_returns_complete_lines_and_continuation() {
        let page = paginate_text("one\ntwo\nthree\n", Some(1), None, 8).expect("page");
        assert_eq!(page.content, "one\ntwo\n");
        assert_eq!(page.start_line, 1);
        assert_eq!(page.end_line, 2);
        assert_eq!(page.total_lines, 3);
        assert_eq!(page.next_start_line, Some(3));
        assert!(page.truncated);
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
            uri: "skill://anchor/example/SKILL.md".into(),
            digest: "sha256:test".into(),
            instruction_lines: 1,
            instruction_chars: 7,
            instruction_bytes: 7,
            estimated_tokens: 2,
            oversized: false,
            quality_warnings: Vec::new(),
            resources: Vec::new(),
            scripts: Vec::new(),
            script_execution_enabled: false,
            script_execution_policy: "operator-dangerous-mode".into(),
            resource_truncated: false,
            warnings: Vec::new(),
        };
        summary.resolve_tools(&["read_file".into(), "mcp_probe_kit__start_feature".into()]);
        assert_eq!(summary.resolved_tools.len(), 2);
        assert_eq!(summary.missing_tools, vec!["missing"]);
        assert!(!summary.tool_compatible);
    }
}
