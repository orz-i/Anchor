use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::model::{
    paginate_text, parse_skill_markdown, SkillLoadResult, SkillReadResult, SkillSummary,
};
use super::resource::{discover_files, read_resource, ResourceReadRequest, MAX_RESOURCE_BYTES};

const DEFAULT_SKILL_ROOTS: &str = ".agents/skills\n.codex/skills\nskills";
const MAX_SKILLS: usize = 200;
pub(super) const MAX_SKILL_MD_BYTES: u64 = 256 * 1024;
pub(super) const MAX_NATIVE_IMPORT_SKILLS: usize = 5;
const MAX_NATIVE_IMPORT_FILES: usize = 100;
const MAX_NATIVE_IMPORT_BYTES: u64 = 5 * 1024 * 1024;
// OpenAI caps the generated archives for a single Scan Tools pass at 8 MiB,
// including ZIP overhead. Keep a conservative raw-payload margin so Anchor
// never advertises a catalog that only fails later during plugin submission.
const MAX_NATIVE_SCAN_RAW_BYTES: u64 = 7 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSettings {
    pub enabled: bool,
    pub roots: Vec<String>,
}

fn copy_skill_tree(record: &SkillRecord, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| format!("无法创建 Skill 导出目录：{error}"))?;
    for entry in WalkDir::new(&record.directory)
        .min_depth(1)
        .max_depth(16)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.map_err(|error| format!("遍历 Skill 目录失败：{error}"))?;
        let relative = entry
            .path()
            .strip_prefix(&record.directory)
            .map_err(|_| "Skill 导出路径无法映射到源目录".to_string())?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "Plugin Skill 快照不接受符号链接：{}",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("无法创建 Skill 子目录：{error}"))?;
            continue;
        }
        if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("无法创建 Skill 文件父目录：{error}"))?;
            }
            fs::copy(entry.path(), &target).map_err(|error| {
                format!(
                    "无法复制 Skill 文件 {}：{error}",
                    relative.to_string_lossy().replace('\\', "/")
                )
            })?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct NativeSkillEntry {
    value: Value,
    total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct NativeSkillCatalog {
    pub skills: Vec<Value>,
    pub eligible_count: usize,
    pub omitted_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSkillExport {
    pub skills: Vec<String>,
    pub total_bytes: u64,
    pub catalog_digest: String,
    pub warnings: Vec<String>,
}

fn is_full_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn script_match(record: &SkillRecord, relative: &str, script_path: &Path) -> SkillScriptMatch {
    let snapshot_digest = record
        .summary
        .scripts
        .iter()
        .find(|script| script.path == relative)
        .map(|script| script.digest.clone())
        .unwrap_or_default();
    let current_digest = current_file_digest(script_path).unwrap_or_else(|| "unreadable".into());
    let reviewable = is_full_sha256(&snapshot_digest);
    SkillScriptMatch {
        skill: record.summary.name.clone(),
        path: relative.to_string(),
        stale: current_digest != snapshot_digest,
        reviewable,
        snapshot_digest,
        current_digest,
    }
}

fn current_file_digest(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_RESOURCE_BYTES {
        return Some(format!("sha256:oversize-{}", metadata.len()));
    }
    let data = fs::read(path).ok()?;
    Some(format!("sha256:{:x}", Sha256::digest(data)))
}

fn normalize_command_path(value: &str) -> String {
    let normalized = value.trim_start_matches(r"\\?\").replace('\\', "/");
    #[cfg(windows)]
    {
        normalized.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

fn public_roots(roots: &[String]) -> Vec<String> {
    roots.iter().map(|root| display_root(root)).collect()
}

impl Default for SkillSettings {
    fn default() -> Self {
        Self::from_text(true, DEFAULT_SKILL_ROOTS)
    }
}

impl SkillSettings {
    pub fn from_text(enabled: bool, roots: &str) -> Self {
        let roots = roots
            .lines()
            .flat_map(|line| line.split(';'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        Self {
            enabled,
            roots: if roots.is_empty() {
                DEFAULT_SKILL_ROOTS.lines().map(str::to_string).collect()
            } else {
                roots
            },
        }
    }

    pub fn roots_text(&self) -> String {
        self.roots.join("\n")
    }
}

#[derive(Debug)]
pub struct SkillCatalog {
    workspace_root: PathBuf,
    settings: RwLock<SkillSettings>,
    snapshot: RwLock<Arc<SkillSnapshot>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillListResult {
    pub enabled: bool,
    pub roots: Vec<String>,
    pub skills: Vec<SkillSummary>,
    pub warnings: Vec<String>,
    pub truncated: bool,
    pub script_execution_enabled: bool,
    pub script_execution_policy: String,
    pub snapshot_mode: String,
    pub catalog_digest: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillScriptMatch {
    pub skill: String,
    pub path: String,
    pub snapshot_digest: String,
    pub current_digest: String,
    pub stale: bool,
    pub reviewable: bool,
}

#[derive(Debug, Clone)]
struct SkillRecord {
    summary: SkillSummary,
    directory: PathBuf,
    raw: String,
    frontmatter: Value,
    skill_md_digest: String,
    instructions: String,
    readable_paths: HashSet<String>,
    readable_digests: std::collections::HashMap<String, String>,
    script_paths: Vec<(String, PathBuf)>,
}

#[derive(Debug, Default)]
struct SkillSnapshot {
    records: BTreeMap<String, SkillRecord>,
    warnings: Vec<String>,
    truncated: bool,
    digest: String,
}

impl SkillCatalog {
    pub fn new(workspace_root: PathBuf) -> Self {
        let settings = SkillSettings::default();
        let snapshot = Arc::new(scan_workspace(&workspace_root, &settings));
        Self {
            workspace_root,
            settings: RwLock::new(settings),
            snapshot: RwLock::new(snapshot),
        }
    }

    pub fn configure(&self, settings: SkillSettings) {
        let snapshot = Arc::new(scan_workspace(&self.workspace_root, &settings));
        *self.settings.write().expect("skill settings write") = settings;
        *self.snapshot.write().expect("skill snapshot write") = snapshot;
    }

    pub fn settings(&self) -> SkillSettings {
        self.settings.read().expect("skill settings read").clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.read().expect("skill settings read").enabled
    }

    pub fn list(&self, query: Option<&str>, max_results: usize) -> SkillListResult {
        let settings = self.settings();
        let snapshot = self.refresh_snapshot();
        if !settings.enabled {
            return SkillListResult {
                enabled: false,
                roots: public_roots(&settings.roots),
                skills: Vec::new(),
                warnings: Vec::new(),
                truncated: false,
                script_execution_enabled: false,
                script_execution_policy: "disabled".into(),
                snapshot_mode: "live-refresh".into(),
                catalog_digest: snapshot.digest.clone(),
            };
        }

        let query = query.map(str::trim).filter(|value| !value.is_empty());
        let mut skills = snapshot
            .records
            .values()
            .map(|record| record.summary.clone())
            .filter(|skill| {
                query.is_none_or(|query| {
                    let query = query.to_ascii_lowercase();
                    skill.name.to_ascii_lowercase().contains(&query)
                        || skill.description.to_ascii_lowercase().contains(&query)
                })
            })
            .collect::<Vec<_>>();
        let limit = max_results.clamp(1, MAX_SKILLS);
        let truncated = snapshot.truncated || skills.len() > limit;
        skills.truncate(limit);

        SkillListResult {
            enabled: true,
            roots: public_roots(&settings.roots),
            skills,
            warnings: snapshot.warnings.clone(),
            truncated,
            script_execution_enabled: false,
            script_execution_policy: "operator-dangerous-mode".into(),
            snapshot_mode: "live-refresh".into(),
            catalog_digest: snapshot.digest.clone(),
        }
    }

    pub fn load(
        &self,
        name: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
        max_bytes: u64,
    ) -> Result<SkillLoadResult, String> {
        let record = self.find(name)?;
        let limit = max_bytes.clamp(1, MAX_SKILL_MD_BYTES) as usize;
        let page = paginate_text(&record.instructions, start_line, end_line, limit)?;
        Ok(SkillLoadResult {
            summary: record.summary,
            instructions: page.content,
            start_line: page.start_line,
            end_line: page.end_line,
            total_lines: page.total_lines,
            total_bytes: page.total_bytes,
            returned_bytes: page.returned_bytes,
            truncated: page.truncated,
            next_start_line: page.next_start_line,
        })
    }

    pub fn read_resource(
        &self,
        name: &str,
        relative_path: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
        max_bytes: u64,
    ) -> Result<SkillReadResult, String> {
        let record = self.find(name)?;
        read_resource(ResourceReadRequest {
            skill_name: &record.summary.name,
            skill_directory: &record.directory,
            readable_paths: &record.readable_paths,
            readable_digests: &record.readable_digests,
            relative_path,
            start_line,
            end_line,
            max_bytes,
        })
    }

    pub fn index_json(&self) -> Result<String, String> {
        let settings = self.settings();
        let snapshot = self.refresh_snapshot();
        let skills = if settings.enabled {
            snapshot
                .records
                .values()
                .map(|record| {
                    serde_json::json!({
                        "name": record.summary.name,
                        "type": "skill-md",
                        "description": record.summary.description,
                        "url": record.summary.uri,
                        "digest": record.summary.digest
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        serde_json::to_string_pretty(&serde_json::json!({
            "$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
            "catalogDigest": snapshot.digest,
            "snapshotMode": "live-refresh",
            "skills": skills
        }))
        .map_err(|error| format!("Skill index 序列化失败：{error}"))
    }

    pub fn native_catalog(&self) -> NativeSkillCatalog {
        let settings = self.settings();
        let snapshot = self.refresh_snapshot();
        if !settings.enabled {
            return NativeSkillCatalog {
                skills: Vec::new(),
                eligible_count: 0,
                omitted_count: 0,
                warnings: Vec::new(),
            };
        }

        let mut eligible = Vec::new();
        let mut warnings = snapshot.warnings.clone();
        for record in snapshot.records.values() {
            match native_entry_with_size(record) {
                Ok(entry) => eligible.push(entry),
                Err(error) => warnings.push(format!(
                    "Skill {} 未进入原生 MCP Skill 导入目录：{error}",
                    record.summary.name
                )),
            }
        }
        let eligible_count = eligible.len();
        let mut exposed = Vec::new();
        let mut exposed_bytes = 0_u64;
        let mut scan_budget_omitted = 0_usize;
        let mut count_omitted = 0_usize;
        for entry in eligible {
            if exposed.len() >= MAX_NATIVE_IMPORT_SKILLS {
                count_omitted += 1;
                continue;
            }
            if exposed_bytes.saturating_add(entry.total_bytes) > MAX_NATIVE_SCAN_RAW_BYTES {
                scan_budget_omitted += 1;
                continue;
            }
            exposed_bytes = exposed_bytes.saturating_add(entry.total_bytes);
            exposed.push(entry.value);
        }
        let omitted_count = eligible_count.saturating_sub(exposed.len());
        if count_omitted > 0 {
            warnings.push(format!(
                "原生 MCP Skill 导入最多暴露 {MAX_NATIVE_IMPORT_SKILLS} 个 Skill；其余 {count_omitted} 个仅保留在 Anchor 本地 Skill 目录中"
            ));
        }
        if scan_budget_omitted > 0 {
            warnings.push(format!(
                "为给 8 MiB Plugin Scan Tools 归档上限预留 ZIP 开销，Anchor 将原生 Skill 原始资源总量限制为 {MAX_NATIVE_SCAN_RAW_BYTES} 字节；有 {scan_budget_omitted} 个 Skill 因扫描总预算未暴露"
            ));
        }
        NativeSkillCatalog {
            skills: exposed,
            eligible_count,
            omitted_count,
            warnings,
        }
    }

    /// Export a static Agent Skills snapshot suitable for a ChatGPT/Codex
    /// plugin `skills/` directory. This is intentionally separate from the
    /// runtime MCP Skills extension: current ChatGPT plugins package Skill
    /// folders in `.codex-plugin/plugin.json` rather than discovering them
    /// dynamically from `skills/list` during ordinary chats.
    pub fn export_plugin_skills(&self, destination: &Path) -> Result<PluginSkillExport, String> {
        let settings = self.settings();
        if !settings.enabled {
            return Err("当前 workspace/profile 未启用 Skill 服务".into());
        }
        let snapshot = self.refresh_snapshot();
        fs::create_dir_all(destination)
            .map_err(|error| format!("无法创建 Plugin skills 目录：{error}"))?;

        let mut exported = Vec::new();
        let mut total_bytes = 0_u64;
        let mut warnings = snapshot.warnings.clone();
        for record in snapshot.records.values() {
            if record.summary.source != "workspace" {
                warnings.push(format!(
                    "Skill {} 来源为 {}，Plugin package 默认只快照 workspace 内 Skill，已跳过",
                    record.summary.name, record.summary.source
                ));
                continue;
            }
            let entry = match native_entry_with_size(record) {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!(
                        "Skill {} 未进入 Plugin 静态快照：{error}",
                        record.summary.name
                    ));
                    continue;
                }
            };
            let target = destination.join(&record.summary.name);
            copy_skill_tree(record, &target)?;
            total_bytes = total_bytes.saturating_add(entry.total_bytes);
            exported.push(record.summary.name.clone());
        }

        if exported.is_empty() {
            return Err(format!(
                "当前 workspace 没有可打包的 Skill{}",
                if warnings.is_empty() {
                    String::new()
                } else {
                    format!("：{}", warnings.join("；"))
                }
            ));
        }

        Ok(PluginSkillExport {
            skills: exported,
            total_bytes,
            catalog_digest: snapshot.digest.clone(),
            warnings,
        })
    }

    pub fn read_skill_markdown(
        &self,
        name: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
        max_bytes: u64,
    ) -> Result<SkillReadResult, String> {
        let record = self.find(name)?;
        let limit = max_bytes.clamp(1, MAX_SKILL_MD_BYTES) as usize;
        let page = paginate_text(&record.raw, start_line, end_line, limit)?;
        Ok(SkillReadResult {
            skill: record.summary.name.clone(),
            path: "SKILL.md".into(),
            uri: record.summary.uri.clone(),
            mime_type: "text/markdown".into(),
            encoding: "utf-8".into(),
            content: page.content,
            size_bytes: page.total_bytes,
            returned_bytes: page.returned_bytes,
            total_lines: Some(page.total_lines),
            start_line: Some(page.start_line),
            end_line: Some(page.end_line),
            truncated: page.truncated,
            next_start_line: page.next_start_line,
        })
    }

    pub fn match_script_command(&self, command: &str, workdir: &Path) -> Option<SkillScriptMatch> {
        if !self.is_enabled() {
            return None;
        }
        // Script execution keeps the configured snapshot as the approval
        // boundary. A modified script must become stale until the operator
        // explicitly reconfigures/restarts the catalog.
        let snapshot = self.snapshot.read().expect("skill snapshot read").clone();
        let normalized_command = normalize_command_path(command);
        let canonical_workdir = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        for record in snapshot.records.values() {
            for (relative, script_path) in &record.script_paths {
                let absolute = normalize_command_path(&script_path.to_string_lossy());
                let relative_normalized = normalize_command_path(relative);
                let workspace_relative = script_path
                    .strip_prefix(
                        self.workspace_root
                            .canonicalize()
                            .unwrap_or_else(|_| self.workspace_root.clone()),
                    )
                    .ok()
                    .map(|path| normalize_command_path(&path.to_string_lossy()));
                if normalized_command.contains(&absolute)
                    || workspace_relative
                        .as_ref()
                        .is_some_and(|path| normalized_command.contains(path))
                    || (canonical_workdir.starts_with(&record.directory)
                        && normalized_command.contains(&relative_normalized))
                    || (script_path.parent() == Some(canonical_workdir.as_path())
                        && script_path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(normalize_command_path)
                            .is_some_and(|name| normalized_command.contains(&name)))
                {
                    return Some(script_match(record, relative, script_path));
                }
            }
        }

        let tokens = shell_words::split(command)
            .unwrap_or_else(|_| command.split_whitespace().map(str::to_string).collect());
        for token in tokens {
            if token.starts_with('-') || token.contains('=') && !token.contains(['/', '\\']) {
                continue;
            }
            let token_path = PathBuf::from(token.replace('/', std::path::MAIN_SEPARATOR_STR));
            let candidate = if token_path.is_absolute() {
                token_path
            } else {
                workdir.join(token_path)
            };
            let Ok(candidate) = candidate.canonicalize() else {
                continue;
            };
            for record in snapshot.records.values() {
                for (relative, script_path) in &record.script_paths {
                    if &candidate == script_path {
                        return Some(script_match(record, relative, script_path));
                    }
                }
            }
        }
        None
    }

    fn find(&self, name: &str) -> Result<SkillRecord, String> {
        if !self.is_enabled() {
            return Err("当前 workspace/profile 未启用 Skill 服务".into());
        }
        self.refresh_snapshot()
            .records
            .get(name.trim())
            .cloned()
            .ok_or_else(|| format!("找不到 Skill：{}", name.trim()))
    }

    fn refresh_snapshot(&self) -> Arc<SkillSnapshot> {
        let settings = self.settings.read().expect("skill settings read");
        let refreshed = Arc::new(scan_workspace(&self.workspace_root, &settings));
        let mut snapshot = self.snapshot.write().expect("skill snapshot write");
        if snapshot.digest != refreshed.digest {
            *snapshot = refreshed;
        }
        snapshot.clone()
    }
}

fn scan_workspace(workspace_root: &Path, settings: &SkillSettings) -> SkillSnapshot {
    if !settings.enabled {
        return SkillSnapshot {
            digest: "sha256:disabled".into(),
            ..SkillSnapshot::default()
        };
    }
    let mut result = SkillSnapshot::default();
    let mut seen_dirs = HashSet::new();
    for configured_root in &settings.roots {
        let resolved = resolve_root(workspace_root, configured_root);
        let canonical_root = match resolved.canonicalize() {
            Ok(path) if path.is_dir() => path,
            Ok(_) => {
                result.warnings.push(format!(
                    "Skill 根目录不是目录：{}",
                    display_root(configured_root)
                ));
                continue;
            }
            Err(_) => continue,
        };
        if !seen_dirs.insert(canonical_root.clone()) {
            continue;
        }

        let candidates = discover_skill_dirs(&canonical_root);
        for skill_dir in candidates {
            if result.records.len() >= MAX_SKILLS {
                result.truncated = true;
                break;
            }
            match read_skill_record(workspace_root, configured_root, &canonical_root, &skill_dir) {
                Ok(record) => {
                    if result.records.contains_key(&record.summary.name) {
                        result.warnings.push(format!(
                            "发现重复 Skill {}，保留先出现的来源，忽略后续目录",
                            record.summary.name
                        ));
                    } else {
                        result.records.insert(record.summary.name.clone(), record);
                    }
                }
                Err(error) => result.warnings.push(format!(
                    "忽略 Skill 目录 {}：{error}",
                    skill_dir
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("<invalid>")
                )),
            }
        }
    }
    let mut digest = Sha256::new();
    for record in result.records.values() {
        digest.update(record.summary.name.as_bytes());
        digest.update([0]);
        digest.update(record.summary.digest.as_bytes());
        digest.update([0]);
    }
    result.digest = format!("sha256:{:x}", digest.finalize());
    result
}

fn discover_skill_dirs(root: &Path) -> Vec<PathBuf> {
    if root.join("SKILL.md").is_file() {
        return vec![root.to_path_buf()];
    }
    let mut dirs = WalkDir::new(root)
        .min_depth(1)
        .max_depth(2)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir() && entry.path().join("SKILL.md").is_file())
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn read_skill_record(
    workspace_root: &Path,
    configured_root: &str,
    source_root: &Path,
    skill_dir: &Path,
) -> Result<SkillRecord, String> {
    let canonical_dir = skill_dir
        .canonicalize()
        .map_err(|error| format!("无法解析目录：{error}"))?;
    if !canonical_dir.starts_with(source_root) {
        return Err("Skill 目录通过符号链接越过配置根目录".into());
    }
    let directory_name = canonical_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Skill 目录名不是有效 UTF-8".to_string())?;
    let skill_md = canonical_dir.join("SKILL.md");
    let metadata = fs::metadata(&skill_md).map_err(|error| format!("缺少 SKILL.md：{error}"))?;
    if metadata.len() > MAX_SKILL_MD_BYTES {
        return Err(format!("SKILL.md 超过 {MAX_SKILL_MD_BYTES} 字节限制"));
    }
    let raw = fs::read_to_string(&skill_md)
        .map_err(|error| format!("SKILL.md 不是可读 UTF-8 文本：{error}"))?;
    let parsed = parse_skill_markdown(&raw, directory_name)?;
    let discovered = discover_files(&canonical_dir);
    let digest = package_digest(&raw, &discovered.resources, &discovered.scripts);
    let skill_md_digest = format!("sha256:{:x}", Sha256::digest(raw.as_bytes()));
    let (source, source_id, relative_path) =
        source_descriptor(workspace_root, configured_root, source_root, &canonical_dir);
    let uri = super::resource::skill_resource_uri(&parsed.name, "SKILL.md");
    let summary = SkillSummary {
        name: parsed.name,
        description: parsed.description,
        license: parsed.license,
        compatibility: parsed.compatibility,
        metadata: parsed.metadata,
        allowed_tools: parsed.allowed_tools,
        resolved_tools: Vec::new(),
        missing_tools: Vec::new(),
        ambiguous_tools: Vec::new(),
        tool_resolution: Vec::new(),
        tool_dependencies_evaluated: false,
        tool_compatible: true,
        tool_enforcement_mode: "declarative-only".into(),
        tool_grants_permissions: false,
        source,
        source_id,
        relative_path,
        uri,
        digest,
        instruction_lines: parsed.instruction_lines,
        instruction_chars: parsed.instruction_chars,
        instruction_bytes: parsed.instruction_bytes,
        estimated_tokens: parsed.estimated_tokens,
        oversized: parsed.oversized,
        quality_warnings: parsed.quality_warnings,
        resources: discovered.resources,
        scripts: discovered.scripts,
        script_execution_enabled: false,
        script_execution_policy: "operator-dangerous-mode".into(),
        resource_truncated: discovered.truncated,
        warnings: discovered.warnings,
    };
    Ok(SkillRecord {
        summary,
        directory: canonical_dir,
        raw,
        frontmatter: parsed.frontmatter,
        skill_md_digest,
        instructions: parsed.instructions,
        readable_paths: discovered.readable_paths,
        readable_digests: discovered.readable_digests,
        script_paths: discovered.script_paths,
    })
}

fn validate_native_manifest_completeness(record: &SkillRecord) -> Result<(), String> {
    const MAX_NATIVE_WALK_DEPTH: usize = 16;
    for entry in WalkDir::new(&record.directory)
        .min_depth(1)
        .max_depth(MAX_NATIVE_WALK_DEPTH)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.map_err(|error| format!("遍历 Skill 目录失败：{error}"))?;
        let relative = entry
            .path()
            .strip_prefix(&record.directory)
            .map_err(|_| "Skill 文件路径无法映射到导入根目录".to_string())?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "原生导入不接受符号链接：{}",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
        if entry.file_type().is_dir() {
            if entry.depth() == MAX_NATIVE_WALK_DEPTH {
                return Err(format!(
                    "Skill 目录深度超过原生导入审计上限 {MAX_NATIVE_WALK_DEPTH}：{}",
                    relative.to_string_lossy().replace('\\', "/")
                ));
            }
            continue;
        }
        if !entry.file_type().is_file() || relative == Path::new("SKILL.md") {
            continue;
        }
        let path = relative.to_string_lossy().replace('\\', "/");
        if !record.readable_paths.contains(&path) {
            return Err(format!(
                "支持文件未进入可校验资源清单或被安全策略排除：{path}"
            ));
        }
    }
    Ok(())
}

fn native_entry_with_size(record: &SkillRecord) -> Result<NativeSkillEntry, String> {
    if record.summary.resource_truncated {
        return Err("资源清单已截断，无法保证导入快照完整".into());
    }
    if !record.summary.warnings.is_empty() {
        return Err(format!(
            "资源扫描存在未完整纳入导入快照的文件：{}",
            record.summary.warnings.join("；")
        ));
    }
    validate_native_manifest_completeness(record)?;
    let mut supporting = record
        .summary
        .resources
        .iter()
        .chain(record.summary.scripts.iter())
        .collect::<Vec<_>>();
    supporting.sort_by(|left, right| left.path.cmp(&right.path));
    if supporting.len().saturating_add(1) > MAX_NATIVE_IMPORT_FILES {
        return Err(format!(
            "文件数 {} 超过原生导入上限 {MAX_NATIVE_IMPORT_FILES}",
            supporting.len().saturating_add(1)
        ));
    }
    if supporting
        .iter()
        .any(|file| !file.readable || !is_full_sha256(&file.digest))
    {
        return Err("存在超过 1 MiB、不可读或缺少完整 SHA-256 的支持文件".into());
    }
    let total_bytes =
        record.raw.len() as u64 + supporting.iter().map(|file| file.size_bytes).sum::<u64>();
    if total_bytes > MAX_NATIVE_IMPORT_BYTES {
        return Err(format!(
            "资源总大小 {total_bytes} 字节超过原生导入上限 {MAX_NATIVE_IMPORT_BYTES}"
        ));
    }

    let mut resources = Vec::with_capacity(supporting.len() + 1);
    resources.push(json!({
        "uri": record.summary.uri,
        "digest": record.skill_md_digest
    }));
    resources.extend(supporting.into_iter().map(|file| {
        json!({
            "uri": super::resource::skill_resource_uri(&record.summary.name, &file.path),
            "digest": file.digest
        })
    }));
    Ok(NativeSkillEntry {
        value: json!({
            "uri": record.summary.uri,
            "frontmatter": record.frontmatter,
            "resources": resources
        }),
        total_bytes,
    })
}

fn package_digest(
    skill_md: &str,
    resources: &[super::model::SkillFileSummary],
    scripts: &[super::model::SkillFileSummary],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"SKILL.md\0");
    digest.update(skill_md.as_bytes());
    digest.update([0]);
    let mut files = resources.iter().chain(scripts.iter()).collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    for file in files {
        digest.update(file.path.as_bytes());
        digest.update([0]);
        digest.update(file.digest.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn source_descriptor(
    workspace_root: &Path,
    configured_root: &str,
    source_root: &Path,
    skill_dir: &Path,
) -> (String, String, String) {
    let workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let home = dirs::home_dir().and_then(|path| path.canonicalize().ok());
    let source = if source_root.starts_with(&workspace) {
        "workspace"
    } else if home
        .as_ref()
        .is_some_and(|home| source_root.starts_with(home))
    {
        "home"
    } else {
        "external"
    };
    let mut digest = Sha256::new();
    digest.update(source_root.to_string_lossy().as_bytes());
    let hex = format!("{:x}", digest.finalize());
    let source_id = if source == "workspace" && !Path::new(configured_root).is_absolute() {
        let label = configured_root
            .trim_matches(['.', '/', '\\'])
            .replace(['/', '\\'], "-");
        format!(
            "workspace-{}",
            if label.is_empty() { "root" } else { &label }
        )
    } else {
        format!("{source}-{}", &hex[..12])
    };
    let relative_path = skill_dir
        .strip_prefix(source_root)
        .unwrap_or_else(|_| Path::new(skill_dir.file_name().unwrap_or_default()))
        .to_string_lossy()
        .replace('\\', "/");
    (
        source.into(),
        source_id,
        if relative_path.is_empty() {
            ".".into()
        } else {
            relative_path
        },
    )
}

fn display_root(configured: &str) -> String {
    if Path::new(configured).is_absolute() {
        "<absolute-skill-root>".into()
    } else {
        configured.to_string()
    }
}

fn resolve_root(workspace_root: &Path, configured: &str) -> PathBuf {
    if configured == "~" {
        return dirs::home_dir().unwrap_or_else(|| workspace_root.to_path_buf());
    }
    if let Some(rest) = configured
        .strip_prefix("~/")
        .or_else(|| configured.strip_prefix("~\\"))
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, description: &str) {
        let directory = root.join(name);
        fs::create_dir_all(directory.join("references")).expect("create skill");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\nUse it.\n"),
        )
        .expect("write skill");
        fs::write(
            directory.join("references/DETAILS.md"),
            "line 1\nline 2\nline 3\n",
        )
        .expect("write resource");
    }

    #[test]
    fn discovers_loads_and_reads_workspace_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "code-review", "Review code safely.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let listed = catalog.list(None, 100);
        assert_eq!(listed.skills.len(), 1);
        assert_eq!(listed.skills[0].name, "code-review");
        assert_eq!(listed.skills[0].resources.len(), 1);
        assert_eq!(listed.skills[0].source, "workspace");
        assert!(!listed.skills[0]
            .source_id
            .contains(temp.path().to_string_lossy().as_ref()));

        let loaded = catalog
            .load("code-review", None, None, 1024)
            .expect("load skill");
        assert!(loaded.instructions.contains("Use it"));
        assert!(loaded.summary.digest.starts_with("sha256:"));
        assert!(!loaded.truncated);

        let resource = catalog
            .read_resource(
                "code-review",
                "references/DETAILS.md",
                Some(2),
                Some(3),
                1024,
            )
            .expect("read resource");
        assert_eq!(resource.content, "line 2\nline 3");
        assert_eq!(resource.encoding, "utf-8");
    }

    #[test]
    fn plugin_export_only_packages_workspace_skill_sources() {
        let workspace = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external");
        write_skill(external.path(), "private-skill", "Private external skill.");
        let catalog = SkillCatalog::new(workspace.path().to_path_buf());
        catalog.configure(SkillSettings {
            enabled: true,
            roots: vec![external.path().to_string_lossy().to_string()],
        });

        let error = catalog
            .export_plugin_skills(&workspace.path().join("plugin-skills"))
            .expect_err("external skill must not be packaged");
        assert!(error.contains("没有可打包的 Skill"));
        assert!(error.contains("Plugin package 默认只快照 workspace 内 Skill"));
        assert!(!workspace
            .path()
            .join("plugin-skills/private-skill/SKILL.md")
            .exists());
    }

    #[test]
    fn discovers_skills_with_extension_frontmatter_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("skills/extended");
        fs::create_dir_all(&skill_dir).expect("skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: extended\ndescription: Extended frontmatter.\nrisk: low\ncategory: development\nuser-invocable: true\n---\nUse it.\n",
        )
        .expect("skill");

        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        let listed = catalog.list(None, 100);

        assert_eq!(listed.skills.len(), 1, "{:?}", listed.warnings);
        assert!(listed.warnings.is_empty(), "{:?}", listed.warnings);
        assert_eq!(listed.skills[0].metadata["risk"], "low");
        assert_eq!(listed.skills[0].metadata["category"], "development");
        assert_eq!(listed.skills[0].metadata["user-invocable"], true);

        let native = catalog.native_catalog();
        assert_eq!(native.skills.len(), 1);
        assert_eq!(native.skills[0]["frontmatter"]["risk"], "low");
        assert_eq!(native.skills[0]["frontmatter"]["category"], "development");
        assert_eq!(native.skills[0]["frontmatter"]["user-invocable"], true);
    }

    #[test]
    fn native_catalog_is_bounded_to_chatgpt_import_limits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        for index in 0..6 {
            write_skill(
                &skills,
                &format!("skill-{index}"),
                &format!("Skill {index}."),
            );
        }
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let native = catalog.native_catalog();

        assert_eq!(native.eligible_count, 6);
        assert_eq!(native.skills.len(), MAX_NATIVE_IMPORT_SKILLS);
        assert_eq!(native.omitted_count, 1);
        assert!(native
            .warnings
            .iter()
            .any(|warning| warning.contains("原生 MCP Skill 导入最多暴露")));
    }

    #[test]
    fn native_catalog_omits_skills_with_unreadable_supporting_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "oversize", "Oversize resource.");
        fs::write(
            skills.join("oversize/references/LARGE.bin"),
            vec![0_u8; (MAX_RESOURCE_BYTES + 1) as usize],
        )
        .expect("large resource");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let native = catalog.native_catalog();

        assert!(native.skills.is_empty());
        assert!(native
            .warnings
            .iter()
            .any(|warning| warning.contains("未进入原生 MCP Skill 导入目录")));
    }

    #[test]
    fn native_catalog_reserves_scan_archive_overhead_across_skills() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        for name in ["archive-a", "archive-b"] {
            write_skill(&skills, name, "Large native import payload.");
            let references = skills.join(name).join("references");
            for index in 0..4 {
                fs::write(
                    references.join(format!("payload-{index}.bin")),
                    vec![0_u8; 900 * 1024],
                )
                .expect("payload");
            }
        }
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let native = catalog.native_catalog();

        assert_eq!(native.eligible_count, 2);
        assert_eq!(native.skills.len(), 1);
        assert_eq!(native.omitted_count, 1);
        assert!(native
            .warnings
            .iter()
            .any(|warning| warning.contains("预留 ZIP 开销")));
    }

    #[test]
    fn native_catalog_requires_every_supporting_file_to_be_manifested() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "complete", "Complete manifest.");
        let extra = skills.join("complete/docs");
        fs::create_dir_all(&extra).expect("extra dir");
        fs::write(extra.join("NOTES.md"), "not in Anchor resource manifest\n").expect("extra file");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let native = catalog.native_catalog();

        assert!(native.skills.is_empty());
        assert!(native.warnings.iter().any(|warning| {
            warning.contains("支持文件未进入可校验资源清单") && warning.contains("docs/NOTES.md")
        }));
    }

    #[test]
    fn first_configured_root_wins_duplicate_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_skill(&temp.path().join("one"), "shared", "First skill.");
        write_skill(&temp.path().join("two"), "shared", "Second skill.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        catalog.configure(SkillSettings::from_text(true, "one\ntwo"));

        let listed = catalog.list(None, 100);

        assert_eq!(listed.skills.len(), 1);
        assert_eq!(listed.skills[0].description, "First skill.");
        assert!(listed.warnings.iter().any(|value| value.contains("重复")));
    }

    #[test]
    fn resource_path_must_be_in_snapshot_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_skill(&temp.path().join("skills"), "safe", "Safe skill.");
        fs::write(
            temp.path().join("skills/safe/unlisted.bin"),
            b"secret-but-not-sensitive",
        )
        .expect("unlisted");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let error = catalog
            .read_resource("safe", "unlisted.bin", None, None, 1024)
            .expect_err("manifest only");
        assert!(error.contains("受控清单"));
    }

    #[test]
    fn snapshot_refreshes_when_skill_files_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "one", "One.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        assert_eq!(catalog.list(None, 100).skills.len(), 1);
        write_skill(&skills, "two", "Two.");
        let listed = catalog.list(None, 100);
        assert_eq!(listed.snapshot_mode, "live-refresh");
        assert_eq!(listed.skills.len(), 2);
    }

    #[test]
    fn package_digest_changes_when_supporting_file_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "digest", "Digest.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        let before = catalog.list(None, 100).skills[0].digest.clone();
        fs::write(skills.join("digest/references/DETAILS.md"), "changed").expect("change");
        let after = catalog.list(None, 100).skills[0].digest.clone();
        assert_ne!(before, after);
    }

    #[test]
    fn repository_installed_skill_uses_supported_frontmatter() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(workspace) = manifest.parent().and_then(Path::parent) else {
            return;
        };
        if !workspace
            .join(".agents/skills/mcp-probe-kit/SKILL.md")
            .is_file()
        {
            return;
        }
        let catalog = SkillCatalog::new(workspace.to_path_buf());
        let listed = catalog.list(Some("mcp-probe-kit"), 10);
        assert_eq!(listed.skills.len(), 1, "{:?}", listed.warnings);
        let skill = &listed.skills[0];
        assert_eq!(skill.metadata["mcp-probe-kit-version"], "3.6.11");
        assert!(skill
            .compatibility
            .as_deref()
            .is_some_and(|value| value.contains("3.6.11")));
        assert!(skill.allowed_tools.contains(&"start_feature".to_string()));
        assert!(skill.allowed_tools.contains(&"workflow".to_string()));

        let native = catalog.native_catalog();
        assert_eq!(native.omitted_count, 0, "{:?}", native.warnings);
        let imported = native
            .skills
            .iter()
            .find(|entry| entry["frontmatter"]["name"] == "mcp-probe-kit")
            .expect("repository Skill must enter the native import catalog");
        assert_eq!(imported["uri"], "skill://anchor/mcp-probe-kit/SKILL.md");
        assert!(imported["resources"]
            .as_array()
            .expect("native resources")
            .iter()
            .any(|resource| {
                resource["uri"] == "skill://anchor/mcp-probe-kit/SKILL.md"
                    && is_full_sha256(resource["digest"].as_str().unwrap_or_default())
            }));
    }

    #[test]
    fn load_skill_pages_long_instructions_without_rejecting_the_skill() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("skills/long-skill");
        fs::create_dir_all(&skill_dir).expect("skill dir");
        let body = (1..=700)
            .map(|line| format!("instruction line {line}\n"))
            .collect::<String>();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: long-skill\ndescription: Long skill.\n---\n{body}"),
        )
        .expect("skill");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let listed = catalog.list(None, 10);
        assert_eq!(listed.skills.len(), 1, "{:?}", listed.warnings);
        assert!(listed.skills[0].oversized);
        assert_eq!(listed.skills[0].instruction_lines, 700);

        let first = catalog
            .load("long-skill", Some(1), Some(400), 4096)
            .expect("first page");
        assert!(first.truncated);
        assert!(first.end_line < 400);
        let next = first.next_start_line.expect("continuation");
        let second = catalog
            .load("long-skill", Some(next), None, 131_072)
            .expect("second page");
        assert_eq!(second.start_line, next);
        assert_eq!(second.end_line, 700);
    }

    #[test]
    fn skill_markdown_keeps_the_256_kib_hard_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        let accepted_dir = skills.join("accepted");
        fs::create_dir_all(&accepted_dir).expect("accepted dir");
        let prefix = "---\nname: accepted\ndescription: Accepted.\n---\n";
        let accepted = format!(
            "{prefix}{}",
            "a".repeat(MAX_SKILL_MD_BYTES as usize - prefix.len())
        );
        assert_eq!(accepted.len(), MAX_SKILL_MD_BYTES as usize);
        fs::write(accepted_dir.join("SKILL.md"), accepted).expect("accepted skill");

        let rejected_dir = skills.join("rejected");
        fs::create_dir_all(&rejected_dir).expect("rejected dir");
        let rejected_prefix = "---\nname: rejected\ndescription: Rejected.\n---\n";
        let rejected = format!(
            "{rejected_prefix}{}",
            "b".repeat(MAX_SKILL_MD_BYTES as usize + 1 - rejected_prefix.len())
        );
        fs::write(rejected_dir.join("SKILL.md"), rejected).expect("rejected skill");

        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        let listed = catalog.list(None, 10);
        assert_eq!(
            listed
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["accepted"]
        );
        assert!(listed
            .warnings
            .iter()
            .any(|warning| warning.contains(&MAX_SKILL_MD_BYTES.to_string())));
    }
}
