use std::fs;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use walkdir::WalkDir;

use super::model::{SkillFileSummary, SkillReadResult};

const MAX_RESOURCE_BYTES: u64 = 1_048_576;

pub(super) fn read_resource(
    skill_name: &str,
    skill_directory: &Path,
    relative_path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: u64,
) -> Result<SkillReadResult, String> {
    let relative = safe_relative_path(relative_path)?;
    let target = skill_directory.join(&relative);
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("Skill 资源不存在：{}（{error}）", target.display()))?;
    let directory = skill_directory
        .canonicalize()
        .map_err(|error| format!("无法解析 Skill 目录：{error}"))?;
    if !canonical.starts_with(&directory) || !canonical.is_file() {
        return Err("Skill 资源路径越界或不是普通文件".into());
    }

    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("无法读取 Skill 资源元数据：{error}"))?;
    let limit = max_bytes.clamp(1, MAX_RESOURCE_BYTES);
    if metadata.len() > limit {
        return Err(format!(
            "Skill 资源大小为 {} 字节，超过 max_bytes={limit}",
            metadata.len()
        ));
    }
    let data = fs::read(&canonical).map_err(|error| format!("读取 Skill 资源失败：{error}"))?;
    let mime_type = mime_type_for(&canonical).to_string();
    let path = slash_path(&relative);
    let uri = format!("skill://{skill_name}/{path}");

    if is_text_path(&canonical) {
        let text = String::from_utf8(data)
            .map_err(|_| "Skill 文本资源不是有效 UTF-8；请通过二进制资源 URI 读取".to_string())?;
        let (content, actual_start, actual_end, truncated) =
            slice_lines(&text, start_line, end_line);
        Ok(SkillReadResult {
            skill: skill_name.to_string(),
            path,
            uri,
            mime_type,
            encoding: "utf-8".into(),
            content,
            size_bytes: metadata.len(),
            start_line: Some(actual_start),
            end_line: Some(actual_end),
            truncated,
        })
    } else {
        if start_line.is_some() || end_line.is_some() {
            return Err("二进制 Skill 资源不支持 start_line/end_line".into());
        }
        Ok(SkillReadResult {
            skill: skill_name.to_string(),
            path,
            uri,
            mime_type,
            encoding: "base64".into(),
            content: base64::engine::general_purpose::STANDARD.encode(data),
            size_bytes: metadata.len(),
            start_line: None,
            end_line: None,
            truncated: false,
        })
    }
}

pub(super) fn discover_files(
    skill_dir: &Path,
) -> (Vec<SkillFileSummary>, Vec<SkillFileSummary>) {
    let mut resources = Vec::new();
    let mut scripts = Vec::new();
    for entry in WalkDir::new(skill_dir)
        .min_depth(1)
        .max_depth(3)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let Ok(relative) = entry.path().strip_prefix(skill_dir) else {
            continue;
        };
        if relative == Path::new("SKILL.md") {
            continue;
        }
        let path = slash_path(relative);
        let is_script = path.starts_with("scripts/");
        let is_resource = path.starts_with("references/")
            || path.starts_with("assets/")
            || (!path.contains('/') && is_text_path(entry.path()));
        if !is_script && !is_resource {
            continue;
        }
        let size_bytes = entry.metadata().map(|value| value.len()).unwrap_or(0);
        let item = SkillFileSummary {
            path,
            kind: if is_script { "script" } else { "resource" }.into(),
            size_bytes,
            mime_type: mime_type_for(entry.path()).into(),
            readable: size_bytes <= MAX_RESOURCE_BYTES,
        };
        if is_script {
            scripts.push(item);
        } else {
            resources.push(item);
        }
    }
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    scripts.sort_by(|left, right| left.path.cmp(&right.path));
    (resources, scripts)
}

fn safe_relative_path(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path.trim());
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("Skill 资源 path 必须是非空相对路径".into());
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Skill 资源 path 不允许 .、..、根目录或前缀".into());
    }
    Ok(path.to_path_buf())
}

fn slice_lines(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> (String, usize, usize, bool) {
    let lines = text.lines().collect::<Vec<_>>();
    let start = start_line.unwrap_or(1).max(1).min(lines.len().max(1));
    let end = end_line
        .unwrap_or(lines.len().max(1))
        .max(start)
        .min(lines.len().max(1));
    let content = if lines.is_empty() {
        String::new()
    } else {
        lines[start - 1..end].join("\n")
    };
    (content, start, end, start > 1 || end < lines.len())
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_text_path(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "md" | "txt" | "json" | "yaml" | "yml" | "csv" | "xml" | "toml" | "ini"
            | "py" | "js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx" | "html"
            | "css" | "scss" | "less" | "svelte" | "vue" | "sh" | "ps1" | "cs"
            | "csx" | "rs" | "go" | "java" | "kt" | "kts" | "rb" | "php" | "sql"
            | "graphql" | "gql" | "env" | "conf"
    )
}

fn mime_type_for(path: &Path) -> &'static str {
    match extension(path).as_str() {
        "md" => "text/markdown",
        "txt" | "py" | "js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx" | "html"
        | "css" | "scss" | "less" | "svelte" | "vue" | "sh" | "ps1" | "cs"
        | "csx" | "rs" | "go" | "java" | "kt" | "kts" | "rb" | "php" | "sql"
        | "graphql" | "gql" | "env" | "conf" | "ini" => "text/plain",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "toml" => "application/toml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_source_files_are_returned_as_text() {
        for file in ["script.py", "component.tsx", "tool.rs", "style.css"] {
            assert!(is_text_path(Path::new(file)), "{file}");
        }
    }
}
