use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::tools::workspace::WorkspaceError;

const SUPPORTED_KINDS: [&str; 4] = ["java", "node", "flutter", "android_sdk"];

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolchainOverlay {
    pub(crate) path_entries: Vec<PathBuf>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) selected: Value,
}

pub(crate) fn registry_summary(registry: &Value) -> Value {
    let mut runtimes = serde_json::Map::new();
    for kind in SUPPORTED_KINDS {
        let candidates = registry
            .pointer(&format!("/runtimes/{kind}"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let active = candidates
            .iter()
            .filter(|candidate| candidate.get("active") == Some(&Value::Bool(true)))
            .cloned()
            .collect::<Vec<_>>();
        runtimes.insert(
            kind.to_string(),
            json!({
                "available_count": candidates.len(),
                "active_count": active.len(),
                "active": active.into_iter().map(|candidate| json!({
                    "version": candidate["version"],
                    "source": candidate["source"]
                })).collect::<Vec<_>>()
            }),
        );
    }
    json!({
        "schema_version": registry["schema_version"],
        "selection_model": registry["selection_model"],
        "accepts_external_paths": registry["accepts_external_paths"],
        "runtimes": runtimes
    })
}

#[derive(Debug, Clone)]
struct RuntimeCandidate {
    kind: &'static str,
    home: PathBuf,
    version: Option<String>,
    source: &'static str,
    active: bool,
    path_entries: Vec<PathBuf>,
    env: BTreeMap<String, String>,
}

pub(crate) fn validate_request(value: Option<&Value>) -> Result<(), WorkspaceError> {
    let Some(value) = value else {
        return Ok(());
    };
    let request = value.as_object().ok_or_else(|| {
        WorkspaceError::invalid_argument("toolchains must be an object of symbolic selectors")
    })?;
    if request.len() > SUPPORTED_KINDS.len() {
        return Err(WorkspaceError::invalid_argument(
            "toolchains contains too many runtime selectors",
        ));
    }
    for (kind, selector) in request {
        if !SUPPORTED_KINDS.contains(&kind.as_str()) {
            return Err(actionable_error(
                "TOOLCHAIN_KIND_UNSUPPORTED",
                format!("Unsupported named toolchain: {kind}"),
                "validation",
                false,
                json!({
                    "cause_scope": "toolchain_registry",
                    "workspace_mutated": false,
                    "toolchain": kind,
                    "supported_toolchains": SUPPORTED_KINDS,
                    "recommended_retry": {
                        "tool": "environment",
                        "arguments": {"operation": "check", "detail": "full"}
                    }
                }),
            ));
        }
        let selector = selector.as_str().ok_or_else(|| {
            WorkspaceError::invalid_argument("toolchain selectors must be strings")
        })?;
        let selector = selector.trim();
        if selector.is_empty() || selector.len() > 64 || !safe_selector(selector) {
            return Err(WorkspaceError::invalid_argument(
                "toolchain selectors must be `default` or a bounded version selector without path syntax",
            ));
        }
    }
    Ok(())
}

pub(crate) fn resolve_requested(execution: &Value) -> Result<ToolchainOverlay, WorkspaceError> {
    validate_request(execution.get("toolchains"))?;
    let Some(request) = execution.get("toolchains").and_then(Value::as_object) else {
        return Ok(ToolchainOverlay::default());
    };
    ensure_no_env_conflicts(execution, request)?;

    let registry = discover_registry();
    let mut overlay = ToolchainOverlay {
        selected: Value::Object(Default::default()),
        ..ToolchainOverlay::default()
    };
    for kind in SUPPORTED_KINDS {
        let Some(selector) = request.get(kind).and_then(Value::as_str) else {
            continue;
        };
        let candidates = registry.get(kind).cloned().unwrap_or_default();
        let selected = select_candidate(kind, selector, &candidates)?;
        for path in &selected.path_entries {
            push_unique(&mut overlay.path_entries, path.clone());
        }
        for (name, value) in &selected.env {
            overlay.env.insert(name.clone(), value.clone());
        }
        overlay.selected[kind] = json!({
            "selector": selector,
            "home": selected.home.display().to_string(),
            "version": selected.version,
            "source": selected.source,
            "active": selected.active,
            "path_entries": selected.path_entries.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "env_keys": selected.env.keys().cloned().collect::<Vec<_>>()
        });
    }
    Ok(overlay)
}

pub(crate) fn registry_value() -> Value {
    let registry = discover_registry();
    let mut runtimes = serde_json::Map::new();
    for kind in SUPPORTED_KINDS {
        let candidates = registry.get(kind).cloned().unwrap_or_default();
        runtimes.insert(
            kind.to_string(),
            Value::Array(candidates.iter().map(candidate_view).collect()),
        );
    }
    json!({
        "schema_version": 1,
        "selection_model": "symbolic_trusted_runtime",
        "accepts_external_paths": false,
        "supported": {
            "java": {"selectors": ["default", "version-prefix"], "sets_env": ["JAVA_HOME"]},
            "node": {"selectors": ["default", "version-prefix"], "sets_env": []},
            "flutter": {"selectors": ["default", "version-prefix"], "sets_env": ["FLUTTER_ROOT"]},
            "android_sdk": {"selectors": ["default"], "sets_env": ["ANDROID_SDK_ROOT", "ANDROID_HOME"]}
        },
        "runtimes": runtimes
    })
}

fn ensure_no_env_conflicts(
    execution: &Value,
    request: &serde_json::Map<String, Value>,
) -> Result<(), WorkspaceError> {
    let Some(env) = execution.get("env").and_then(Value::as_object) else {
        return Ok(());
    };
    let mut conflicts = Vec::new();
    for (kind, names) in [
        ("java", &["JAVA_HOME"][..]),
        ("flutter", &["FLUTTER_ROOT", "FLUTTER_HOME"][..]),
        ("android_sdk", &["ANDROID_SDK_ROOT", "ANDROID_HOME"][..]),
    ] {
        if request.contains_key(kind) {
            conflicts.extend(
                names
                    .iter()
                    .filter(|name| env.contains_key(**name))
                    .copied(),
            );
        }
    }
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(actionable_error(
        "TOOLCHAIN_ENV_CONFLICT",
        "Named toolchains cannot be combined with environment variables that select the same runtime.",
        "validation",
        false,
        json!({
            "cause_scope": "toolchain_registry",
            "workspace_mutated": false,
            "conflicting_env": conflicts,
            "recommended_retry": {
                "tool": "exec_command",
                "remove_env_keys": conflicts,
                "preserve": ["toolchains"]
            }
        }),
    ))
}

fn select_candidate(
    kind: &'static str,
    selector: &str,
    candidates: &[RuntimeCandidate],
) -> Result<RuntimeCandidate, WorkspaceError> {
    let selector = selector.trim();
    let matching = candidates
        .iter()
        .filter(|candidate| selector_matches(selector, candidate.version.as_deref()))
        .cloned()
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(selection_error(
            "TOOLCHAIN_NOT_FOUND",
            kind,
            selector,
            candidates,
            "No trusted runtime matches the requested toolchain selector.",
        ));
    }
    let active = matching
        .iter()
        .filter(|candidate| candidate.active)
        .cloned()
        .collect::<Vec<_>>();
    if active.len() == 1 {
        return Ok(active[0].clone());
    }
    if active.len() > 1 {
        return Err(selection_error(
            "TOOLCHAIN_AMBIGUOUS",
            kind,
            selector,
            &matching,
            "Multiple active trusted runtimes match the requested selector.",
        ));
    }
    if matching.len() == 1 {
        return Ok(matching[0].clone());
    }
    Err(selection_error(
        "TOOLCHAIN_AMBIGUOUS",
        kind,
        selector,
        &matching,
        "Multiple trusted runtimes match the requested selector and none is the unique active runtime.",
    ))
}

fn selection_error(
    code: &'static str,
    kind: &str,
    selector: &str,
    candidates: &[RuntimeCandidate],
    message: &str,
) -> WorkspaceError {
    actionable_error(
        code,
        message,
        "runtime",
        true,
        json!({
            "cause_scope": "toolchain_registry",
            "workspace_mutated": false,
            "toolchain": kind,
            "selector": selector,
            "candidates": candidates.iter().map(candidate_view).collect::<Vec<_>>(),
            "recommended_retry": {
                "tool": "environment",
                "arguments": {"operation": "check", "detail": "full"},
                "inspect": format!("development_environment.toolchain_registry.runtimes.{kind}")
            }
        }),
    )
}

fn actionable_error(
    code: &'static str,
    message: impl Into<String>,
    category: &'static str,
    retryable: bool,
    details: Value,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category,
        retryable,
        details,
    }
}

fn safe_selector(value: &str) -> bool {
    value == "default"
        || value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn selector_matches(selector: &str, version: Option<&str>) -> bool {
    if selector.eq_ignore_ascii_case("default") {
        return true;
    }
    let Some(version) = version else {
        return false;
    };
    let selector = selector.trim_start_matches(['v', 'V']);
    let version = version.trim_start_matches(['v', 'V']);
    version == selector
        || version.strip_prefix(selector).is_some_and(|rest| {
            rest.starts_with('.') || rest.starts_with('-') || rest.starts_with('_')
        })
}

fn discover_registry() -> BTreeMap<&'static str, Vec<RuntimeCandidate>> {
    let mut registry = BTreeMap::new();
    registry.insert("java", discover_java());
    registry.insert("node", discover_node());
    registry.insert("flutter", discover_flutter());
    registry.insert("android_sdk", discover_android_sdk());
    registry
}

fn discover_java() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME").map(PathBuf::from) {
        add_java_home(&mut candidates, home, "process_env", true);
    }
    if let Some(javac) = crate::tools::exec::resolve_effective_system_program("javac") {
        if let Some(home) = javac.parent().and_then(Path::parent) {
            add_java_home(&mut candidates, home.to_path_buf(), "path", true);
        }
    }
    for root in java_installation_roots() {
        scan_children(&root, 64, |path| {
            add_java_home(
                &mut candidates,
                path.clone(),
                "standard_installation",
                false,
            );
            add_java_home(
                &mut candidates,
                path.join("Contents/Home"),
                "standard_installation",
                false,
            );
        });
    }
    candidates
}

fn add_java_home(
    candidates: &mut Vec<RuntimeCandidate>,
    home: PathBuf,
    source: &'static str,
    active: bool,
) {
    let bin = home.join("bin");
    if find_program(&bin, "java").is_none() || find_program(&bin, "javac").is_none() {
        return;
    }
    let version = java_version(&home);
    let mut env = BTreeMap::new();
    env.insert("JAVA_HOME".into(), home.display().to_string());
    add_candidate(
        candidates,
        RuntimeCandidate {
            kind: "java",
            home,
            version,
            source,
            active,
            path_entries: vec![bin],
            env,
        },
    );
}

fn discover_node() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    let active_home = crate::tools::exec::resolve_effective_system_program("node")
        .as_deref()
        .and_then(program_home_from_bin);
    if let Some(home) = active_home.clone() {
        add_node_home(&mut candidates, home, "path", true);
    }
    if let Some(nvm_root) = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".nvm")))
    {
        scan_children(&nvm_root.join("versions/node"), 128, |home| {
            let active = active_home
                .as_ref()
                .is_some_and(|current| same_path(current, home));
            add_node_home(&mut candidates, home.clone(), "nvm", active);
        });
    }
    if let Some(volta) = std::env::var_os("VOLTA_HOME").map(PathBuf::from) {
        let bin = volta.join("bin");
        if find_program(&bin, "node").is_some() {
            let home = volta;
            let active = active_home
                .as_ref()
                .is_some_and(|current| same_path(&current.join("bin"), &bin));
            add_node_home(&mut candidates, home, "volta", active);
        }
    }
    candidates
}

fn add_node_home(
    candidates: &mut Vec<RuntimeCandidate>,
    home: PathBuf,
    source: &'static str,
    active: bool,
) {
    let bin = home.join("bin");
    if find_program(&bin, "node").is_none() {
        return;
    }
    let version = path_version(&home);
    add_candidate(
        candidates,
        RuntimeCandidate {
            kind: "node",
            home,
            version,
            source,
            active,
            path_entries: vec![bin],
            env: BTreeMap::new(),
        },
    );
}

fn discover_flutter() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    for name in ["FLUTTER_ROOT", "FLUTTER_HOME"] {
        if let Some(home) = std::env::var_os(name).map(PathBuf::from) {
            add_flutter_home(&mut candidates, home, "process_env", true);
        }
    }
    let active_home = crate::tools::exec::resolve_effective_system_program("flutter")
        .as_deref()
        .and_then(program_home_from_bin);
    if let Some(home) = active_home {
        add_flutter_home(&mut candidates, home, "path", true);
    }
    if let Some(home) = dirs::home_dir() {
        for candidate in [
            home.join("flutter"),
            home.join("development/flutter"),
            home.join("fvm/default"),
        ] {
            add_flutter_home(&mut candidates, candidate, "standard_installation", false);
        }
    }
    candidates
}

fn add_flutter_home(
    candidates: &mut Vec<RuntimeCandidate>,
    home: PathBuf,
    source: &'static str,
    active: bool,
) {
    let bin = home.join("bin");
    if find_program(&bin, "flutter").is_none() {
        return;
    }
    let version = std::fs::read_to_string(home.join("version"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| path_version(&home));
    let mut env = BTreeMap::new();
    env.insert("FLUTTER_ROOT".into(), home.display().to_string());
    add_candidate(
        candidates,
        RuntimeCandidate {
            kind: "flutter",
            home,
            version,
            source,
            active,
            path_entries: vec![bin],
            env,
        },
    );
}

fn discover_android_sdk() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();
    for name in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(home) = std::env::var_os(name).map(PathBuf::from) {
            add_android_home(&mut candidates, home, "process_env", true);
        }
    }
    if let Some(adb) = crate::tools::exec::resolve_effective_system_program("adb") {
        if let Some(home) = adb.parent().and_then(Path::parent) {
            add_android_home(&mut candidates, home.to_path_buf(), "path", true);
        }
    }
    if let Some(home) = dirs::home_dir() {
        for candidate in [home.join("Android/Sdk"), home.join("Library/Android/sdk")] {
            add_android_home(&mut candidates, candidate, "standard_installation", false);
        }
    }
    #[cfg(windows)]
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        add_android_home(
            &mut candidates,
            local_app_data.join("Android/Sdk"),
            "standard_installation",
            false,
        );
    }
    candidates
}

fn add_android_home(
    candidates: &mut Vec<RuntimeCandidate>,
    home: PathBuf,
    source: &'static str,
    active: bool,
) {
    let platform_tools = home.join("platform-tools");
    let has_sdk = find_program(&platform_tools, "adb").is_some()
        || home.join("platforms").is_dir()
        || home.join("cmdline-tools").is_dir();
    if !has_sdk {
        return;
    }
    let mut path_entries = Vec::new();
    for path in [
        platform_tools,
        home.join("cmdline-tools/latest/bin"),
        home.join("emulator"),
    ] {
        if path.is_dir() {
            push_unique(&mut path_entries, path);
        }
    }
    let mut env = BTreeMap::new();
    env.insert("ANDROID_SDK_ROOT".into(), home.display().to_string());
    env.insert("ANDROID_HOME".into(), home.display().to_string());
    add_candidate(
        candidates,
        RuntimeCandidate {
            kind: "android_sdk",
            home,
            version: None,
            source,
            active,
            path_entries,
            env,
        },
    );
}

fn add_candidate(candidates: &mut Vec<RuntimeCandidate>, candidate: RuntimeCandidate) {
    if let Some(existing) = candidates
        .iter_mut()
        .find(|existing| same_path(&existing.home, &candidate.home))
    {
        let was_active = existing.active;
        existing.active |= candidate.active;
        if candidate.active && !was_active {
            existing.source = candidate.source;
        }
        if existing.version.is_none() {
            existing.version = candidate.version;
        }
        for path in candidate.path_entries {
            push_unique(&mut existing.path_entries, path);
        }
        for (name, value) in candidate.env {
            existing.env.entry(name).or_insert(value);
        }
        return;
    }
    candidates.push(candidate);
}

fn candidate_view(candidate: &RuntimeCandidate) -> Value {
    json!({
        "kind": candidate.kind,
        "home": candidate.home.display().to_string(),
        "version": candidate.version,
        "source": candidate.source,
        "active": candidate.active,
        "path_entries": candidate.path_entries.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "env_keys": candidate.env.keys().cloned().collect::<Vec<_>>()
    })
}

fn java_installation_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "linux")]
    roots.extend([
        PathBuf::from("/usr/lib/jvm"),
        PathBuf::from("/opt/java"),
        PathBuf::from("/opt/jdk"),
    ]);
    #[cfg(target_os = "macos")]
    roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
    #[cfg(windows)]
    {
        for name in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(name).map(PathBuf::from) {
                roots.extend([
                    root.join("Java"),
                    root.join("Eclipse Adoptium"),
                    root.join("Microsoft"),
                ]);
            }
        }
    }
    roots
}

fn java_version(home: &Path) -> Option<String> {
    let release = std::fs::read_to_string(home.join("release")).ok()?;
    release.lines().find_map(|line| {
        line.strip_prefix("JAVA_VERSION=")
            .map(|value| value.trim().trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
    })
}

fn path_version(home: &Path) -> Option<String> {
    home.file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| value.bytes().any(|byte| byte.is_ascii_digit()))
        .map(|value| value.trim_start_matches(['v', 'V']).to_string())
}

fn program_home_from_bin(program: &Path) -> Option<PathBuf> {
    let parent = program.parent()?;
    if parent
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("bin"))
    {
        return parent.parent().map(Path::to_path_buf);
    }
    Some(parent.to_path_buf())
}

fn scan_children(root: &Path, limit: usize, mut visit: impl FnMut(&PathBuf)) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten().take(limit) {
        if entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
            visit(&entry.path());
        }
    }
}

fn find_program(bin: &Path, name: &str) -> Option<PathBuf> {
    #[cfg(not(windows))]
    let candidates = vec![bin.join(name)];
    #[cfg(windows)]
    let mut candidates = vec![bin.join(name)];
    #[cfg(windows)]
    candidates.extend([
        bin.join(format!("{name}.exe")),
        bin.join(format!("{name}.bat")),
        bin.join(format!("{name}.cmd")),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|current| same_path(current, &path)) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        kind: &'static str,
        home: &str,
        version: Option<&str>,
        active: bool,
    ) -> RuntimeCandidate {
        RuntimeCandidate {
            kind,
            home: PathBuf::from(home),
            version: version.map(str::to_string),
            source: "test",
            active,
            path_entries: vec![PathBuf::from(home).join("bin")],
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn symbolic_selector_never_accepts_path_syntax() {
        for invalid in ["/opt/jdk", "../jdk", r"C:\\Java\\jdk", ""] {
            let error =
                validate_request(Some(&json!({"java": invalid}))).expect_err("invalid selector");
            assert_eq!(error.to_error_value()["code"], "INVALID_ARGUMENT");
        }
    }

    #[test]
    fn default_prefers_the_unique_active_runtime() {
        let candidates = vec![
            candidate("java", "/one", Some("17.0.12"), false),
            candidate("java", "/two", Some("21.0.4"), true),
        ];
        let selected = select_candidate("java", "default", &candidates).expect("active runtime");
        assert_eq!(selected.home, PathBuf::from("/two"));
    }

    #[test]
    fn version_selector_is_fail_closed_when_multiple_inactive_runtimes_match() {
        let candidates = vec![
            candidate("java", "/one", Some("17.0.10"), false),
            candidate("java", "/two", Some("17.0.12"), false),
        ];
        let error = select_candidate("java", "17", &candidates).expect_err("ambiguous");
        let value = error.to_error_value();
        assert_eq!(value["code"], "TOOLCHAIN_AMBIGUOUS");
        assert_eq!(value["details"]["cause_scope"], "toolchain_registry");
        assert_eq!(value["details"]["workspace_mutated"], false);
    }

    #[test]
    fn registry_summary_hides_candidate_paths_but_keeps_active_runtime_state() {
        let registry = json!({
            "schema_version": 1,
            "selection_model": "symbolic_trusted_runtime",
            "accepts_external_paths": false,
            "runtimes": {
                "java": [{"home": "/secret-ish/path", "version": "17.0.12", "source": "path", "active": true}],
                "node": [],
                "flutter": [],
                "android_sdk": []
            }
        });
        let summary = registry_summary(&registry);
        assert_eq!(summary["runtimes"]["java"]["available_count"], 1);
        assert_eq!(summary["runtimes"]["java"]["active_count"], 1);
        assert_eq!(
            summary["runtimes"]["java"]["active"][0]["version"],
            "17.0.12"
        );
        assert!(!summary.to_string().contains("secret-ish"));
    }
}
