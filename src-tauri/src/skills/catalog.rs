use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::model::{parse_skill_markdown, SkillLoadResult, SkillReadResult, SkillSummary};
use super::resource::{discover_files, read_resource};

const DEFAULT_SKILL_ROOTS: &str = ".agents/skills\n.codex/skills\nskills";
const MAX_SKILLS: usize = 200;
const MAX_SKILL_MD_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone)]
pub struct SkillSettings {
    pub enabled: bool,
    pub roots: Vec<String>,
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
}

#[derive(Debug)]
struct SkillRecord {
    summary: SkillSummary,
    directory: PathBuf,
    raw: String,
    instructions: String,
}

#[derive(Debug, Default)]
struct ScanResult {
    records: BTreeMap<String, SkillRecord>,
    warnings: Vec<String>,
    truncated: bool,
}

impl SkillCatalog {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            settings: RwLock::new(SkillSettings::default()),
        }
    }

    pub fn configure(&self, settings: SkillSettings) {
        *self.settings.write().expect("skill settings write") = settings;
    }

    pub fn settings(&self) -> SkillSettings {
        self.settings.read().expect("skill settings read").clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.read().expect("skill settings read").enabled
    }

    pub fn list(&self, query: Option<&str>, max_results: usize) -> SkillListResult {
        let settings = self.settings();
        if !settings.enabled {
            return SkillListResult {
                enabled: false,
                roots: settings.roots,
                skills: Vec::new(),
                warnings: Vec::new(),
                truncated: false,
                script_execution_enabled: false,
            };
        }

        let scan = self.scan(&settings);
        let query = query.map(str::trim).filter(|value| !value.is_empty());
        let mut skills = scan
            .records
            .into_values()
            .map(|record| record.summary)
            .filter(|skill| {
                query.is_none_or(|query| {
                    let query = query.to_ascii_lowercase();
                    skill.name.to_ascii_lowercase().contains(&query)
                        || skill.description.to_ascii_lowercase().contains(&query)
                })
            })
            .collect::<Vec<_>>();
        let limit = max_results.clamp(1, MAX_SKILLS);
        let truncated = scan.truncated || skills.len() > limit;
        skills.truncate(limit);

        SkillListResult {
            enabled: true,
            roots: settings.roots,
            skills,
            warnings: scan.warnings,
            truncated,
            script_execution_enabled: false,
        }
    }

    pub fn load(&self, name: &str, max_bytes: u64) -> Result<SkillLoadResult, String> {
        let record = self.find(name)?;
        let limit = max_bytes.clamp(1, MAX_SKILL_MD_BYTES);
        let size = record.raw.len() as u64;
        if size > limit {
            return Err(format!(
                "Skill {} 的 SKILL.md 大小为 {size} 字节，超过 max_bytes={limit}",
                record.summary.name
            ));
        }
        Ok(SkillLoadResult {
            summary: record.summary,
            skill_md: record.raw,
            instructions: record.instructions,
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
        read_resource(
            &record.summary.name,
            &record.directory,
            relative_path,
            start_line,
            end_line,
            max_bytes,
        )
    }

    pub fn index_json(&self) -> Result<String, String> {
        let settings = self.settings();
        let scan = if settings.enabled {
            self.scan(&settings)
        } else {
            ScanResult::default()
        };
        let skills = scan
            .records
            .into_values()
            .map(|record| {
                serde_json::json!({
                    "name": record.summary.name,
                    "type": "skill-md",
                    "description": record.summary.description,
                    "url": record.summary.uri,
                    "digest": record.summary.digest
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&serde_json::json!({
            "$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
            "skills": skills
        }))
        .map_err(|error| format!("Skill index 序列化失败：{error}"))
    }

    fn find(&self, name: &str) -> Result<SkillRecord, String> {
        let settings = self.settings();
        if !settings.enabled {
            return Err("当前 workspace/profile 未启用 Skill 服务".into());
        }
        let mut scan = self.scan(&settings);
        scan.records
            .remove(name.trim())
            .ok_or_else(|| format!("找不到 Skill：{}", name.trim()))
    }

    fn scan(&self, settings: &SkillSettings) -> ScanResult {
        let mut result = ScanResult::default();
        let mut seen_dirs = HashSet::new();
        for configured_root in &settings.roots {
            let resolved = resolve_root(&self.workspace_root, configured_root);
            let canonical_root = match resolved.canonicalize() {
                Ok(path) if path.is_dir() => path,
                Ok(_) => {
                    result.warnings.push(format!(
                        "Skill 根目录不是目录：{}",
                        resolved.display()
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
                match read_skill_record(&canonical_root, &skill_dir) {
                    Ok(record) => {
                        if result.records.contains_key(&record.summary.name) {
                            result.warnings.push(format!(
                                "发现重复 Skill {}，保留先出现的目录，忽略 {}",
                                record.summary.name,
                                skill_dir.display()
                            ));
                        } else {
                            result
                                .records
                                .insert(record.summary.name.clone(), record);
                        }
                    }
                    Err(error) => result
                        .warnings
                        .push(format!("忽略 Skill 目录 {}：{error}", skill_dir.display())),
                }
            }
        }
        result
    }
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

fn read_skill_record(source_root: &Path, skill_dir: &Path) -> Result<SkillRecord, String> {
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
        return Err(format!(
            "SKILL.md 超过 {} 字节限制",
            MAX_SKILL_MD_BYTES
        ));
    }
    let raw = fs::read_to_string(&skill_md)
        .map_err(|error| format!("SKILL.md 不是可读 UTF-8 文本：{error}"))?;
    let parsed = parse_skill_markdown(&raw, directory_name)?;
    let (resources, scripts) = discover_files(&canonical_dir);
    let digest = format!("sha256:{:x}", Sha256::digest(raw.as_bytes()));
    let uri = format!("skill://{}/SKILL.md", parsed.name);
    let summary = SkillSummary {
        name: parsed.name,
        description: parsed.description,
        license: parsed.license,
        compatibility: parsed.compatibility,
        metadata: parsed.metadata,
        allowed_tools: parsed.allowed_tools,
        source_root: source_root.display().to_string(),
        skill_dir: canonical_dir.display().to_string(),
        uri,
        digest,
        resources,
        scripts,
        script_execution_enabled: false,
    };
    Ok(SkillRecord {
        summary,
        directory: canonical_dir,
        raw,
        instructions: parsed.instructions,
    })
}

fn resolve_root(workspace_root: &Path, configured: &str) -> PathBuf {
    if configured == "~" {
        return dirs::home_dir().unwrap_or_else(|| workspace_root.to_path_buf());
    }
    if let Some(rest) = configured.strip_prefix("~/").or_else(|| configured.strip_prefix("~\\")) {
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
        fs::write(directory.join("references/DETAILS.md"), "line 1\nline 2\nline 3\n")
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

        let loaded = catalog.load("code-review", 1024).expect("load skill");
        assert!(loaded.instructions.contains("Use it"));
        assert!(loaded.summary.digest.starts_with("sha256:"));

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
    fn resource_path_cannot_escape_skill_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_skill(&temp.path().join("skills"), "safe", "Safe skill.");
        fs::write(temp.path().join("secret.txt"), "secret").expect("secret");
        let catalog = SkillCatalog::new(temp.path().to_path_buf());

        let error = catalog
            .read_resource("safe", "../secret.txt", None, None, 1024)
            .expect_err("escape rejected");

        assert!(error.contains("不允许"));
    }
}
