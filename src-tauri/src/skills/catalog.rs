use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::model::{
    paginate_text, parse_skill_markdown, SkillLoadResult, SkillReadResult, SkillSummary,
};
use super::resource::{discover_files, read_resource, ResourceReadRequest, MAX_RESOURCE_BYTES};

const DEFAULT_SKILL_ROOTS: &str = ".agents/skills\n.codex/skills\nskills";
const MAX_SKILLS: usize = 200;
pub(super) const MAX_SKILL_MD_BYTES: u64 = 131_072;

#[derive(Debug, Clone)]
pub struct SkillSettings {
    pub enabled: bool,
    pub roots: Vec<String>,
}

fn is_full_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
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
        let snapshot = self.snapshot();
        if !settings.enabled {
            return SkillListResult {
                enabled: false,
                roots: public_roots(&settings.roots),
                skills: Vec::new(),
                warnings: Vec::new(),
                truncated: false,
                script_execution_enabled: false,
                script_execution_policy: "disabled".into(),
                snapshot_mode: "listener-fixed".into(),
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
            snapshot_mode: "listener-fixed".into(),
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
        let snapshot = self.snapshot();
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
            "snapshotMode": "listener-fixed",
            "skills": skills
        }))
        .map_err(|error| format!("Skill index 序列化失败：{error}"))
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
        let snapshot = self.snapshot();
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
        self.snapshot()
            .records
            .get(name.trim())
            .cloned()
            .ok_or_else(|| format!("找不到 Skill：{}", name.trim()))
    }

    fn snapshot(&self) -> Arc<SkillSnapshot> {
        self.snapshot.read().expect("skill snapshot read").clone()
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
    let (source, source_id, relative_path) =
        source_descriptor(workspace_root, configured_root, source_root, &canonical_dir);
    let uri = format!("skill://{}/SKILL.md", parsed.name);
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
        instructions: parsed.instructions,
        readable_paths: discovered.readable_paths,
        readable_digests: discovered.readable_digests,
        script_paths: discovered.script_paths,
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
    fn snapshot_does_not_change_until_reconfigured() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "one", "One.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        assert_eq!(catalog.list(None, 100).skills.len(), 1);
        write_skill(&skills, "two", "Two.");
        assert_eq!(catalog.list(None, 100).skills.len(), 1);
        catalog.configure(SkillSettings::default());
        assert_eq!(catalog.list(None, 100).skills.len(), 2);
    }

    #[test]
    fn package_digest_changes_when_supporting_file_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skills = temp.path().join("skills");
        write_skill(&skills, "digest", "Digest.");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());
        let before = catalog.list(None, 100).skills[0].digest.clone();
        fs::write(skills.join("digest/references/DETAILS.md"), "changed").expect("change");
        catalog.configure(SkillSettings::default());
        let after = catalog.list(None, 100).skills[0].digest.clone();
        assert_ne!(before, after);
    }

    #[test]
    fn repository_installed_skill_uses_supported_frontmatter() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(workspace) = manifest.parent() else {
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
    fn skill_markdown_keeps_the_128_kib_hard_boundary() {
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
            .any(|warning| warning.contains("131072")));
    }
}
