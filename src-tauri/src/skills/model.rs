use std::collections::BTreeMap;

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
    pub source_root: String,
    pub skill_dir: String,
    pub uri: String,
    pub digest: String,
    pub resources: Vec<SkillFileSummary>,
    pub scripts: Vec<SkillFileSummary>,
    pub script_execution_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLoadResult {
    #[serde(flatten)]
    pub summary: SkillSummary,
    pub skill_md: String,
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
    allowed_tools: AllowedTools,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum AllowedTools {
    #[default]
    Empty,
    Text(String),
    List(Vec<String>),
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

pub fn parse_skill_markdown(raw: &str, directory_name: &str) -> Result<ParsedSkillMarkdown, String> {
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

    let allowed_tools = match parsed.allowed_tools {
        AllowedTools::Empty => Vec::new(),
        AllowedTools::Text(value) => value
            .split_whitespace()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        AllowedTools::List(values) => values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
    };
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
            "Skill name 只能包含小写字母、数字和单个连字符，且不能以连字符开头或结尾"
                .into(),
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
}
