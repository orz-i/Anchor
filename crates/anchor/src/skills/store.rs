use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;

const SKILL_STORE_SCHEMA_VERSION: u32 = 1;
const MAX_ACTIVATION_HISTORY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillChannel {
    Stable,
    Development,
    Canary,
    Pinned,
}

impl SkillChannel {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "stable" => Ok(Self::Stable),
            "development" => Ok(Self::Development),
            "canary" => Ok(Self::Canary),
            "pinned" => Ok(Self::Pinned),
            other => Err(format!(
                "invalid Skill channel `{other}`; expected stable, development, canary, or pinned"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Development => "development",
            Self::Canary => "canary",
            Self::Pinned => "pinned",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPackageValidation {
    pub name: String,
    pub digest: String,
    pub declared_version: Option<String>,
    pub source_path: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPackageVersionView {
    pub digest: String,
    pub declared_version: Option<String>,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPackageView {
    pub name: String,
    pub versions: Vec<SkillPackageVersionView>,
    pub channels: BTreeMap<String, String>,
    pub active_channel: Option<String>,
    pub active_digest: Option<String>,
    pub rollback_depth: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPackageList {
    pub store_schema_version: u32,
    pub store_root: String,
    pub packages: Vec<SkillPackageView>,
}

#[derive(Debug, Clone)]
pub struct ActiveSkillPackage {
    pub name: String,
    pub digest: String,
    pub declared_version: Option<String>,
    pub channel: String,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledVersion {
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    declared_version: Option<String>,
    installed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationRecord {
    channel: String,
    digest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageState {
    #[serde(default)]
    versions: BTreeMap<String, InstalledVersion>,
    #[serde(default)]
    channels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_digest: Option<String>,
    #[serde(default)]
    history: Vec<ActivationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreState {
    schema_version: u32,
    #[serde(default)]
    packages: BTreeMap<String, PackageState>,
}

impl Default for StoreState {
    fn default() -> Self {
        Self {
            schema_version: SKILL_STORE_SCHEMA_VERSION,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct SkillPackageStore {
    root: PathBuf,
    state: RwLock<StoreState>,
    load_error: RwLock<Option<String>>,
}

impl SkillPackageStore {
    pub fn new(workspace_root: &Path) -> Self {
        let root = workspace_root.join(".anchor").join("skills");
        let (state, load_error) = match load_state(&root) {
            Ok(state) => (state, None),
            Err(error) => (StoreState::default(), Some(error)),
        };
        Self {
            root,
            state: RwLock::new(state),
            load_error: RwLock::new(load_error),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error
            .read()
            .expect("skill package load error read")
            .clone()
    }

    pub fn refresh_from_disk(&self) -> Result<bool, String> {
        let loaded = match load_state(&self.root) {
            Ok(state) => state,
            Err(error) => {
                *self
                    .load_error
                    .write()
                    .expect("skill package load error write") = Some(error.clone());
                return Err(error);
            }
        };
        *self
            .load_error
            .write()
            .expect("skill package load error write") = None;
        let mut state = self.state.write().expect("skill package state write");
        if *state == loaded {
            return Ok(false);
        }
        *state = loaded;
        Ok(true)
    }

    pub fn list(&self) -> SkillPackageList {
        let state = self.state.read().expect("skill package state read");
        let packages = state
            .packages
            .iter()
            .map(|(name, package)| SkillPackageView {
                name: name.clone(),
                versions: package
                    .versions
                    .values()
                    .map(|version| SkillPackageVersionView {
                        digest: version.digest.clone(),
                        declared_version: version.declared_version.clone(),
                        installed_at: version.installed_at.clone(),
                    })
                    .collect(),
                channels: package.channels.clone(),
                active_channel: package.active_channel.clone(),
                active_digest: package.active_digest.clone(),
                rollback_depth: package.history.len(),
            })
            .collect();
        SkillPackageList {
            store_schema_version: state.schema_version,
            store_root: ".anchor/skills".into(),
            packages,
        }
    }

    pub fn active_packages(&self) -> Vec<ActiveSkillPackage> {
        let state = self.state.read().expect("skill package state read");
        state
            .packages
            .iter()
            .filter_map(|(name, package)| {
                let digest = package.active_digest.as_ref()?;
                let channel = package.active_channel.as_ref()?;
                let version = package.versions.get(digest)?;
                Some(ActiveSkillPackage {
                    name: name.clone(),
                    digest: digest.clone(),
                    declared_version: version.declared_version.clone(),
                    channel: channel.clone(),
                    directory: self.package_dir(name, digest),
                })
            })
            .collect()
    }

    pub fn install(
        &self,
        source: &Path,
        validation: &SkillPackageValidation,
        channel: SkillChannel,
        activate: bool,
    ) -> Result<SkillPackageView, String> {
        let _mutation_lock = self.acquire_mutation_lock()?;
        self.refresh_from_disk()?;
        self.ensure_healthy()?;
        let target = self.package_dir(&validation.name, &validation.digest);
        let mut installed_new_tree = false;
        if !target.exists() {
            let staging = self.root.join(".staging").join(format!(
                "{}-{}",
                validation.name,
                Uuid::new_v4().simple()
            ));
            if let Err(error) = copy_tree(source, &staging) {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create Skill package parent: {error}"))?;
            }
            fs::rename(&staging, &target).map_err(|error| {
                let _ = fs::remove_dir_all(&staging);
                format!("failed to publish Skill package tree: {error}")
            })?;
            installed_new_tree = true;
        }

        let mut state = self.state.write().expect("skill package state write");
        let before = state.clone();
        let view = {
            let package = state.packages.entry(validation.name.clone()).or_default();
            if let Some(declared) = validation.declared_version.as_ref() {
                if let Some(conflict) = package.versions.values().find(|version| {
                    version.declared_version.as_deref() == Some(declared.as_str())
                        && version.digest != validation.digest
                }) {
                    if installed_new_tree {
                        let _ = fs::remove_dir_all(&target);
                    }
                    return Err(format!(
                        "Skill {} declared version `{declared}` is immutable and already maps to {}",
                        validation.name, conflict.digest
                    ));
                }
            }
            package
                .versions
                .entry(validation.digest.clone())
                .or_insert_with(|| InstalledVersion {
                    digest: validation.digest.clone(),
                    declared_version: validation.declared_version.clone(),
                    installed_at: Utc::now().to_rfc3339(),
                });
            package
                .channels
                .insert(channel.as_str().into(), validation.digest.clone());
            if activate {
                activate_package(package, channel.as_str(), &validation.digest);
            }
            package_view(&validation.name, package)
        };
        if let Err(error) = persist_state(&self.root, &state) {
            *state = before;
            if installed_new_tree {
                let _ = fs::remove_dir_all(&target);
            }
            return Err(error);
        }
        Ok(view)
    }

    pub fn set_channel(
        &self,
        name: &str,
        channel: SkillChannel,
        version_ref: &str,
    ) -> Result<SkillPackageView, String> {
        let _mutation_lock = self.acquire_mutation_lock()?;
        self.refresh_from_disk()?;
        self.ensure_healthy()?;
        self.mutate(name, |package| {
            let digest = resolve_version(package, version_ref)?;
            package.channels.insert(channel.as_str().into(), digest);
            Ok(())
        })
    }

    pub fn activate(&self, name: &str, channel: SkillChannel) -> Result<SkillPackageView, String> {
        let _mutation_lock = self.acquire_mutation_lock()?;
        self.refresh_from_disk()?;
        self.ensure_healthy()?;
        self.mutate(name, |package| {
            let digest = package
                .channels
                .get(channel.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "Skill `{name}` channel `{}` has no installed version",
                        channel.as_str()
                    )
                })?;
            activate_package(package, channel.as_str(), &digest);
            Ok(())
        })
    }

    pub fn rollback(&self, name: &str) -> Result<SkillPackageView, String> {
        let _mutation_lock = self.acquire_mutation_lock()?;
        self.refresh_from_disk()?;
        self.ensure_healthy()?;
        self.mutate(name, |package| {
            while let Some(previous) = package.history.pop() {
                if package.versions.contains_key(&previous.digest) {
                    package.active_channel = Some(previous.channel);
                    package.active_digest = Some(previous.digest);
                    return Ok(());
                }
            }
            Err(format!("Skill `{name}` has no rollback target"))
        })
    }

    pub fn remove(&self, name: &str, version_ref: &str) -> Result<SkillPackageView, String> {
        let _mutation_lock = self.acquire_mutation_lock()?;
        self.refresh_from_disk()?;
        self.ensure_healthy()?;
        let mut state = self.state.write().expect("skill package state write");
        let before = state.clone();
        let package = state
            .packages
            .get_mut(name)
            .ok_or_else(|| format!("Skill package `{name}` is not installed"))?;
        let digest = resolve_version(package, version_ref)?;
        if package.active_digest.as_deref() == Some(digest.as_str()) {
            return Err(format!(
                "Skill `{name}` version `{version_ref}` is active; activate another version before removal"
            ));
        }
        let channel_refs = package
            .channels
            .iter()
            .filter_map(|(channel, target)| (target == &digest).then_some(channel.clone()))
            .collect::<Vec<_>>();
        if !channel_refs.is_empty() {
            return Err(format!(
                "Skill `{name}` version `{version_ref}` is still referenced by channels: {}",
                channel_refs.join(", ")
            ));
        }
        let target = self.package_dir(name, &digest);
        let trash = self
            .root
            .join(".trash")
            .join(format!("{}-{}", name, Uuid::new_v4().simple()));
        let moved = if target.exists() {
            if let Some(parent) = trash.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create Skill package trash: {error}"))?;
            }
            fs::rename(&target, &trash)
                .map_err(|error| format!("failed to stage Skill package removal: {error}"))?;
            true
        } else {
            false
        };
        package.versions.remove(&digest);
        package.history.retain(|entry| entry.digest != digest);
        let view = package_view(name, package);
        if package.versions.is_empty() {
            state.packages.remove(name);
        }
        if let Err(error) = persist_state(&self.root, &state) {
            *state = before;
            if moved {
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::rename(&trash, &target);
            }
            return Err(error);
        }
        if moved {
            let _ = fs::remove_dir_all(&trash);
        }
        Ok(view)
    }

    fn mutate(
        &self,
        name: &str,
        mutation: impl FnOnce(&mut PackageState) -> Result<(), String>,
    ) -> Result<SkillPackageView, String> {
        let mut state = self.state.write().expect("skill package state write");
        let before = state.clone();
        let package = state
            .packages
            .get_mut(name)
            .ok_or_else(|| format!("Skill package `{name}` is not installed"))?;
        mutation(package)?;
        if let Err(error) = persist_state(&self.root, &state) {
            *state = before;
            return Err(error);
        }
        let package = state
            .packages
            .get(name)
            .expect("package remains after mutation");
        Ok(package_view(name, package))
    }

    fn package_dir(&self, name: &str, digest: &str) -> PathBuf {
        self.root
            .join("packages")
            .join(name)
            .join(digest_hex(digest).unwrap_or("invalid"))
            .join(name)
    }

    fn ensure_healthy(&self) -> Result<(), String> {
        if let Some(error) = self.load_error() {
            return Err(format!("Skill package store is unavailable: {error}"));
        }
        Ok(())
    }

    fn acquire_mutation_lock(&self) -> Result<File, String> {
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("failed to create Skill package store: {error}"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("state.lock"))
            .map_err(|error| format!("failed to open Skill package store lock: {error}"))?;
        FileExt::lock_exclusive(&lock)
            .map_err(|error| format!("failed to lock Skill package store: {error}"))?;
        Ok(lock)
    }
}

fn activate_package(package: &mut PackageState, channel: &str, digest: &str) {
    let changed = package.active_digest.as_deref() != Some(digest)
        || package.active_channel.as_deref() != Some(channel);
    if changed {
        if let (Some(previous_channel), Some(previous_digest)) = (
            package.active_channel.clone(),
            package.active_digest.clone(),
        ) {
            package.history.push(ActivationRecord {
                channel: previous_channel,
                digest: previous_digest,
            });
            if package.history.len() > MAX_ACTIVATION_HISTORY {
                let overflow = package.history.len() - MAX_ACTIVATION_HISTORY;
                package.history.drain(..overflow);
            }
        }
        package.active_channel = Some(channel.to_string());
        package.active_digest = Some(digest.to_string());
    }
}

fn resolve_version(package: &PackageState, version_ref: &str) -> Result<String, String> {
    let reference = version_ref.trim();
    if package.versions.contains_key(reference) {
        return Ok(reference.to_string());
    }
    let matches = package
        .versions
        .values()
        .filter(|version| version.declared_version.as_deref() == Some(reference))
        .map(|version| version.digest.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(format!(
            "installed Skill version `{reference}` was not found"
        )),
        _ => Err(format!(
            "installed Skill version `{reference}` is ambiguous; use the full sha256 digest"
        )),
    }
}

fn package_view(name: &str, package: &PackageState) -> SkillPackageView {
    SkillPackageView {
        name: name.to_string(),
        versions: package
            .versions
            .values()
            .map(|version| SkillPackageVersionView {
                digest: version.digest.clone(),
                declared_version: version.declared_version.clone(),
                installed_at: version.installed_at.clone(),
            })
            .collect(),
        channels: package.channels.clone(),
        active_channel: package.active_channel.clone(),
        active_digest: package.active_digest.clone(),
        rollback_depth: package.history.len(),
    }
}

fn digest_hex(digest: &str) -> Result<&str, String> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("invalid Skill package digest `{digest}`"))?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("invalid Skill package digest `{digest}`"));
    }
    Ok(hex)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_dir_all(destination)
            .map_err(|error| format!("failed to clear Skill staging directory: {error}"))?;
    }
    fs::create_dir_all(destination)
        .map_err(|error| format!("failed to create Skill staging directory: {error}"))?;
    for entry in WalkDir::new(source)
        .min_depth(1)
        .max_depth(16)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.map_err(|error| format!("failed to walk Skill source: {error}"))?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|_| "Skill source path escaped its package root".to_string())?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "Skill package installation rejects symbolic links: {}",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("failed to create Skill package directory: {error}"))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("failed to create Skill package file parent: {error}")
                })?;
            }
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("failed to copy Skill package file: {error}"))?;
        }
    }
    Ok(())
}

fn load_state(root: &Path) -> Result<StoreState, String> {
    let path = root.join("state.json");
    if !path.exists() {
        return Ok(StoreState::default());
    }
    let raw =
        fs::read(&path).map_err(|error| format!("failed to read Skill store state: {error}"))?;
    let state: StoreState = serde_json::from_slice(&raw)
        .map_err(|error| format!("failed to parse Skill store state: {error}"))?;
    if state.schema_version != SKILL_STORE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Skill store schema {}; expected {}",
            state.schema_version, SKILL_STORE_SCHEMA_VERSION
        ));
    }
    Ok(state)
}

fn persist_state(root: &Path, state: &StoreState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("failed to serialize Skill store state: {error}"))?;
    crate::data::atomic_write(&root.join("state.json"), &bytes)
        .map_err(|error| format!("failed to persist Skill store state: {error}"))
}
