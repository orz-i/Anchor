use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::model::{SkillFileSummary, SkillReadResult};

pub(super) const MAX_RESOURCE_BYTES: u64 = 1_048_576;
const MAX_FILES_PER_SKILL: usize = 256;
const MAX_WALK_DEPTH: usize = 8;

pub(super) struct DiscoveredFiles {
    pub resources: Vec<SkillFileSummary>,
    pub scripts: Vec<SkillFileSummary>,
    pub readable_paths: HashSet<String>,
    pub readable_digests: HashMap<String, String>,
    pub script_paths: Vec<(String, PathBuf)>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

pub(super) struct ResourceReadRequest<'a> {
    pub skill_name: &'a str,
    pub skill_directory: &'a Path,
    pub readable_paths: &'a HashSet<String>,
    pub readable_digests: &'a HashMap<String, String>,
    pub relative_path: &'a str,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub max_bytes: u64,
}

pub(super) fn read_resource(request: ResourceReadRequest<'_>) -> Result<SkillReadResult, String> {
    let relative = safe_relative_path(request.relative_path)?;
    let path = slash_path(&relative);
    if !request.readable_paths.contains(&path) {
        return Err(format!(
            "Skill 资源未包含在受控清单中或被安全策略排除：{path}"
        ));
    }
    let target = request.skill_directory.join(&relative);
    if fs::symlink_metadata(&target)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!("Skill 资源在快照后被替换为符号链接：{path}"));
    }
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("Skill 资源不存在：{}（{error}）", target.display()))?;
    let directory = request
        .skill_directory
        .canonicalize()
        .map_err(|error| format!("无法解析 Skill 目录：{error}"))?;
    if !canonical.starts_with(&directory) || !canonical.is_file() {
        return Err("Skill 资源路径越界或不是普通文件".into());
    }

    let metadata =
        fs::metadata(&canonical).map_err(|error| format!("无法读取 Skill 资源元数据：{error}"))?;
    let limit = request.max_bytes.clamp(1, MAX_RESOURCE_BYTES);
    if metadata.len() > limit {
        return Err(format!(
            "Skill 资源大小为 {} 字节，超过 max_bytes={limit}",
            metadata.len()
        ));
    }
    let data = fs::read(&canonical).map_err(|error| format!("读取 Skill 资源失败：{error}"))?;
    let current_digest = format!("sha256:{:x}", Sha256::digest(&data));
    if request.readable_digests.get(&path) != Some(&current_digest) {
        return Err(format!(
            "Skill 资源在目录快照建立后已变化：{path}；请重启 MCP listener 重新加载 Skill"
        ));
    }
    let mime_type = mime_type_for(&canonical).to_string();
    let uri = skill_resource_uri(request.skill_name, &path);

    if is_text_path(&canonical) {
        let text = String::from_utf8(data)
            .map_err(|_| "Skill 文本资源不是有效 UTF-8；请通过二进制资源 URI 读取".to_string())?;
        let (content, actual_start, actual_end, total_lines, truncated, next_start_line) =
            slice_lines(&text, request.start_line, request.end_line);
        let returned_bytes = content.len();
        Ok(SkillReadResult {
            skill: request.skill_name.to_string(),
            path,
            uri,
            mime_type,
            encoding: "utf-8".into(),
            content,
            size_bytes: metadata.len(),
            returned_bytes,
            total_lines: Some(total_lines),
            start_line: Some(actual_start),
            end_line: Some(actual_end),
            truncated,
            next_start_line,
        })
    } else {
        if request.start_line.is_some() || request.end_line.is_some() {
            return Err("二进制 Skill 资源不支持 start_line/end_line".into());
        }
        Ok(SkillReadResult {
            skill: request.skill_name.to_string(),
            path,
            uri,
            mime_type,
            encoding: "base64".into(),
            content: base64::engine::general_purpose::STANDARD.encode(data),
            size_bytes: metadata.len(),
            returned_bytes: metadata.len() as usize,
            total_lines: None,
            start_line: None,
            end_line: None,
            truncated: false,
            next_start_line: None,
        })
    }
}

pub(super) fn discover_files(skill_dir: &Path) -> DiscoveredFiles {
    let mut resources = Vec::new();
    let mut scripts = Vec::new();
    let mut readable_paths = HashSet::new();
    let mut readable_digests = HashMap::new();
    let mut script_paths = Vec::new();
    let mut warnings = Vec::new();
    let mut truncated = false;
    let mut accepted = 0usize;

    for entry in WalkDir::new(skill_dir)
        .min_depth(1)
        .max_depth(MAX_WALK_DEPTH)
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
        if sensitive_skill_path(relative) {
            warnings.push(format!("安全策略已排除潜在敏感文件：{path}"));
            continue;
        }
        let is_script = path.starts_with("scripts/");
        let is_resource = path.starts_with("references/")
            || path.starts_with("assets/")
            || (!path.contains('/') && is_text_path(entry.path()));
        if !is_script && !is_resource {
            continue;
        }
        if accepted >= MAX_FILES_PER_SKILL {
            truncated = true;
            break;
        }
        accepted += 1;

        let size_bytes = entry.metadata().map(|value| value.len()).unwrap_or(0);
        let readable = size_bytes <= MAX_RESOURCE_BYTES;
        let digest = if readable {
            fs::read(entry.path())
                .map(|data| format!("sha256:{:x}", Sha256::digest(data)))
                .unwrap_or_else(|_| "sha256:unreadable".into())
        } else {
            warnings.push(format!(
                "文件超过 {} 字节，已列出但禁止读取：{path}",
                MAX_RESOURCE_BYTES
            ));
            format!("sha256:oversize-{size_bytes}")
        };
        let item = SkillFileSummary {
            path: path.clone(),
            kind: if is_script { "script" } else { "resource" }.into(),
            size_bytes,
            mime_type: mime_type_for(entry.path()).into(),
            readable,
            digest: digest.clone(),
        };
        if readable {
            readable_paths.insert(path.clone());
            readable_digests.insert(path.clone(), digest.clone());
        }
        if is_script {
            if let Ok(canonical) = entry.path().canonicalize() {
                script_paths.push((path, canonical));
            }
            scripts.push(item);
        } else {
            resources.push(item);
        }
    }
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    scripts.sort_by(|left, right| left.path.cmp(&right.path));
    script_paths.sort_by(|left, right| left.0.cmp(&right.0));
    if truncated {
        warnings.push(format!(
            "Skill 文件数量超过 {MAX_FILES_PER_SKILL}，其余文件未进入资源清单"
        ));
    }
    DiscoveredFiles {
        resources,
        scripts,
        readable_paths,
        readable_digests,
        script_paths,
        warnings,
        truncated,
    }
}

fn sensitive_skill_path(path: &Path) -> bool {
    if path.components().any(|component| {
        matches!(component, Component::Normal(value) if value.to_string_lossy().starts_with('.'))
    }) {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "credentials"
            | "credentials.json"
            | "credentials.yaml"
            | "credentials.yml"
            | "secrets"
            | "secrets.json"
            | "secrets.yaml"
            | "secrets.yml"
            | "id_rsa"
            | "id_ed25519"
    ) || [
        "credential.",
        "credentials.",
        "secret.",
        "secrets.",
        "token.",
        "tokens.",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || matches!(
            extension.as_str(),
            "key" | "pem" | "p12" | "pfx" | "jks" | "kdbx"
        )
        || [
            "client_secret",
            "private_key",
            "api_key",
            "access_token",
            "refresh_token",
        ]
        .iter()
        .any(|marker| name.contains(marker))
}

fn safe_relative_path(path: &str) -> Result<PathBuf, String> {
    let normalized = path.trim().replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return Err("Skill 资源 path 必须是非空相对路径".into());
    }
    let mut safe = PathBuf::new();
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("Skill 资源 path 不允许 .、..、根目录或前缀".into());
        }
        let component = Path::new(segment).components().next();
        if !matches!(component, Some(Component::Normal(_))) {
            return Err("Skill 资源 path 不允许 .、..、根目录或前缀".into());
        }
        safe.push(segment);
    }
    Ok(safe)
}

fn slice_lines(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> (String, usize, usize, usize, bool, Option<usize>) {
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
    let truncated = start > 1 || end < lines.len();
    let next_start_line = (end < lines.len()).then_some(end + 1);
    (content, start, end, lines.len(), truncated, next_start_line)
}

pub(super) fn skill_resource_uri(skill_name: &str, path: &str) -> String {
    let encoded = path
        .split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("skill://{skill_name}/{encoded}")
}

pub(super) fn percent_decode_path(path: &str) -> Result<String, String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("Skill URI 包含无效百分号编码".into());
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "Skill URI path 不是有效 UTF-8".into())
}

fn percent_encode_segment(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn hex_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("Skill URI 包含无效百分号编码".into()),
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_text_path(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "md" | "txt"
            | "json"
            | "yaml"
            | "yml"
            | "csv"
            | "xml"
            | "toml"
            | "ini"
            | "py"
            | "js"
            | "mjs"
            | "cjs"
            | "ts"
            | "jsx"
            | "tsx"
            | "html"
            | "css"
            | "scss"
            | "less"
            | "svelte"
            | "vue"
            | "sh"
            | "ps1"
            | "cs"
            | "csx"
            | "rs"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "rb"
            | "php"
            | "sql"
            | "graphql"
            | "gql"
            | "conf"
    )
}

fn mime_type_for(path: &Path) -> &'static str {
    match extension(path).as_str() {
        "md" => "text/markdown",
        "txt" | "py" | "js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx" | "html" | "css" | "scss"
        | "less" | "svelte" | "vue" | "sh" | "ps1" | "cs" | "csx" | "rs" | "go" | "java" | "kt"
        | "kts" | "rb" | "php" | "sql" | "graphql" | "gql" | "conf" | "ini" => "text/plain",
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
        assert!(!is_text_path(Path::new(".env")));
    }

    #[test]
    fn hidden_and_secret_files_are_excluded_from_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("references/.hidden")).expect("dirs");
        fs::create_dir_all(temp.path().join("scripts")).expect("scripts");
        fs::write(temp.path().join("references/ok.md"), "ok").expect("ok");
        fs::write(temp.path().join("references/.env"), "TOKEN=x").expect("env");
        fs::write(temp.path().join("scripts/client_secret.py"), "secret").expect("secret");
        let discovered = discover_files(temp.path());
        assert!(discovered.readable_paths.contains("references/ok.md"));
        assert!(!discovered.readable_paths.contains("references/.env"));
        assert!(!discovered
            .readable_paths
            .contains("scripts/client_secret.py"));
        assert_eq!(discovered.warnings.len(), 2);
    }

    #[test]
    fn uri_round_trip_encodes_spaces_and_unicode() {
        let uri = skill_resource_uri("example", "references/中文 file.md");
        assert_eq!(
            uri,
            "skill://example/references/%E4%B8%AD%E6%96%87%20file.md"
        );
        let encoded = uri.split_once("/references/").unwrap().1;
        assert_eq!(
            percent_decode_path(&format!("references/{encoded}")).unwrap(),
            "references/中文 file.md"
        );
    }
}
