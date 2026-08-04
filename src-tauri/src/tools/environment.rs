use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;

pub fn diagnose(root: &Path) -> Value {
    let package = read_package_json(root);
    let declared = declared_package_manager(root, package.as_ref());
    let git = probe("git", &["--version"], root, Duration::from_secs(3));
    let git_line_endings = if git["healthy"] == true {
        git_line_ending_diagnostics(root)
    } else {
        json!({
            "core_autocrlf": null,
            "core_eol": null,
            "attributes_file": root.join(".gitattributes").exists(),
            "healthy": false,
            "error": "git is unavailable"
        })
    };
    let pwsh = probe(
        "pwsh",
        &[
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ],
        root,
        Duration::from_secs(3),
    );
    let windows_powershell = probe(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ],
        root,
        Duration::from_secs(3),
    );
    let go = probe("go", &["version"], root, Duration::from_secs(3));
    let node = probe("node", &["--version"], root, Duration::from_secs(3));
    let pnpm = probe("pnpm", &["--version"], root, Duration::from_secs(3));
    let corepack = probe("corepack", &["--version"], root, Duration::from_secs(3));
    let corepack_pnpm = if corepack["healthy"] == true {
        probe(
            "corepack",
            &["pnpm", "--version"],
            root,
            Duration::from_secs(5),
        )
    } else {
        unavailable_probe("corepack is unavailable")
    };
    let cargo = probe("cargo", &["--version"], root, Duration::from_secs(3));
    let cargo_clippy = probe(
        "cargo",
        &["clippy", "--version"],
        root,
        Duration::from_secs(5),
    );
    let cargo_fmt = probe("cargo", &["fmt", "--version"], root, Duration::from_secs(5));
    let rustc = probe("rustc", &["--version"], root, Duration::from_secs(3));
    let rustup = probe("rustup", &["--version"], root, Duration::from_secs(3));
    let rustup_cargo = if rustup["healthy"] == true {
        probe(
            "rustup",
            &["run", "stable", "cargo", "--version"],
            root,
            Duration::from_secs(5),
        )
    } else {
        unavailable_probe("rustup is unavailable")
    };
    let rustup_rustc_path = if rustup["healthy"] == true {
        probe("rustup", &["which", "rustc"], root, Duration::from_secs(5))
    } else {
        unavailable_probe("rustup is unavailable")
    };
    let docker_cli = probe("docker", &["--version"], root, Duration::from_secs(3));
    let docker_daemon = if docker_cli["healthy"] == true {
        probe(
            "docker",
            &["info", "--format", "{{.ServerVersion}}"],
            root,
            Duration::from_secs(5),
        )
    } else {
        unavailable_probe("docker CLI is unavailable")
    };
    let redirection_trust = redirection_trust_diagnostics();
    let node_modules = node_modules_diagnostics(root, package.as_ref(), &redirection_trust);
    let host_frontend_healthy = node["healthy"] == true
        && package_manager_probe(&declared, &pnpm, &corepack_pnpm)
        && node_modules["traversable"] == true
        && node_modules["direct_packages_healthy"] == true
        && node_modules["required_bins_healthy"] == true;
    let host_rust_healthy = cargo["healthy"] == true
        && cargo_clippy["healthy"] == true
        && cargo_fmt["healthy"] == true
        && rustc["healthy"] == true;
    let host_healthy = host_frontend_healthy && host_rust_healthy;
    let docker_project = has_docker_project(root);
    let requirements = project_requirements(root, package.as_ref(), docker_project);
    let mut missing_required_tools = Vec::new();
    if requirements["git"] == true && git["healthy"] != true {
        missing_required_tools.push("git");
    }
    if requirements["node"] == true && node["healthy"] != true {
        missing_required_tools.push("node");
    }
    if requirements["pnpm"] == true && !package_manager_probe(&declared, &pnpm, &corepack_pnpm) {
        missing_required_tools.push("pnpm");
    }
    if requirements["rust"] == true && !host_rust_healthy {
        missing_required_tools.push("rust-toolchain");
    }
    if requirements["go"] == true && go["healthy"] != true {
        missing_required_tools.push("go");
    }
    if requirements["docker"] == true && docker_daemon["healthy"] != true {
        missing_required_tools.push("docker-daemon");
    }
    if requirements["powershell"] == true
        && pwsh["healthy"] != true
        && windows_powershell["healthy"] != true
    {
        missing_required_tools.push("powershell");
    }
    let recommended_route = if host_healthy {
        "host"
    } else if docker_daemon["healthy"] == true && docker_project {
        "docker"
    } else {
        "repair_host"
    };
    let mut findings = Vec::new();
    if declared["name"] == "pnpm" && pnpm["healthy"] != true {
        findings.push("Host pnpm is unhealthy".to_string());
    }
    if declared["conflicting_lockfiles"] == true {
        findings.push(
            "Multiple active package-manager lockfiles were detected; keep one declared installer"
                .to_string(),
        );
    }
    if cargo["healthy"] != true && rustup_cargo["healthy"] == true {
        findings.push(
            "The cargo proxy is unhealthy, but `rustup run stable cargo` is available".to_string(),
        );
    }
    if cargo["healthy"] == true && cargo_clippy["healthy"] != true {
        findings.push(
            "Cargo cannot execute Clippy; ensure the active Rust toolchain bin directory is on PATH"
                .to_string(),
        );
    }
    if cargo["healthy"] == true && cargo_fmt["healthy"] != true {
        findings.push(
            "Cargo cannot execute rustfmt; ensure the active Rust toolchain bin directory is on PATH"
                .to_string(),
        );
    }
    if rustc["healthy"] != true && rustup_rustc_path["healthy"] == true {
        findings.push(
            "The rustc proxy is unhealthy; set RUSTC to the path returned by `rustup which rustc`"
                .to_string(),
        );
    }
    if node_modules["traversable"] != true {
        findings.push("node_modules contains an unreadable symlink/junction boundary".to_string());
    }
    if node_modules["direct_packages_healthy"] != true {
        findings.push("One or more direct node_modules packages are not traversable".to_string());
    }
    if node_modules["mixed_installer_metadata"] == true {
        findings.push(
            "node_modules contains both npm and pnpm installer metadata; rebuild it with the declared package manager"
                .to_string(),
        );
    }
    if node_modules["redirection_guard_incompatible_layout"] == true {
        findings.push(
            "Windows RedirectionGuard blocks pnpm's isolated symlink layout; configure `nodeLinker: hoisted` and reinstall dependencies"
                .to_string(),
        );
    }
    if recommended_route == "docker" {
        findings.push("Docker frontend verification is healthy and preferred".to_string());
    }
    if git_line_endings["core_autocrlf"] == "true" && !root.join(".gitattributes").exists() {
        findings.push(
            "Git core.autocrlf=true without a repository .gitattributes file can create platform-dependent diffs"
                .to_string(),
        );
    }
    if !missing_required_tools.is_empty() {
        findings.push(format!(
            "Required tools are unavailable or unhealthy: {}",
            missing_required_tools.join(", ")
        ));
    }

    json!({
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "shell": std::env::var("COMSPEC")
                .or_else(|_| std::env::var("SHELL"))
                .unwrap_or_default(),
            "path_separator": if cfg!(windows) { ";" } else { ":" }
        },
        "windows_redirection_trust": redirection_trust,
        "project_requirements": requirements,
        "missing_required_tools": missing_required_tools,
        "git_line_endings": git_line_endings,
        "package_manager": declared,
        "probes": {
            "git": git,
            "pwsh": pwsh,
            "windows_powershell": windows_powershell,
            "go": go,
            "node": node,
            "pnpm": pnpm,
            "corepack": corepack,
            "corepack_pnpm": corepack_pnpm,
            "cargo": cargo,
            "cargo_clippy": cargo_clippy,
            "cargo_fmt": cargo_fmt,
            "rustc": rustc,
            "rustup": rustup,
            "rustup_cargo": rustup_cargo,
            "rustup_rustc_path": rustup_rustc_path,
            "docker_cli": docker_cli,
            "docker_daemon": docker_daemon
        },
        "node_modules": node_modules,
        "docker_project_detected": docker_project,
        "host_frontend_healthy": host_frontend_healthy,
        "host_rust_healthy": host_rust_healthy,
        "host_healthy": host_healthy,
        "recommended_verification_route": recommended_route,
        "findings": findings
    })
}

fn project_requirements(root: &Path, package: Option<&Value>, docker_project: bool) -> Value {
    json!({
        "git": root.join(".git").exists(),
        "node": package.is_some(),
        "pnpm": package
            .and_then(|value| value.get("packageManager"))
            .and_then(Value::as_str)
            .is_some_and(|manager| manager.starts_with("pnpm@"))
            || root.join("pnpm-lock.yaml").exists(),
        "rust": root.join("Cargo.toml").exists() || root.join("src-tauri/Cargo.toml").exists(),
        "go": root.join("go.mod").exists() || root.join("go.work").exists(),
        "docker": docker_project,
        "powershell": cfg!(windows)
    })
}

fn git_line_ending_diagnostics(root: &Path) -> Value {
    let autocrlf = optional_git_config(root, "core.autocrlf");
    let core_eol = optional_git_config(root, "core.eol");
    json!({
        "core_autocrlf": autocrlf["value"],
        "core_eol": core_eol["value"],
        "attributes_file": root.join(".gitattributes").exists(),
        "healthy": autocrlf["healthy"] == true && core_eol["healthy"] == true,
        "error": if autocrlf["healthy"] == true && core_eol["healthy"] == true {
            Value::Null
        } else {
            Value::String("unable to read Git line-ending configuration".into())
        }
    })
}

fn optional_git_config(root: &Path, key: &str) -> Value {
    let result = probe(
        "git",
        &["config", "--get", key],
        root,
        Duration::from_secs(3),
    );
    let exit_code = result.get("exit_code").and_then(Value::as_i64);
    json!({
        "healthy": result["found"] == true && matches!(exit_code, Some(0 | 1)),
        "configured": exit_code == Some(0),
        "value": if exit_code == Some(0) { result["version"].clone() } else { Value::Null }
    })
}

fn read_package_json(root: &Path) -> Option<Value> {
    let bytes = std::fs::read(root.join("package.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn declared_package_manager(root: &Path, package: Option<&Value>) -> Value {
    let declared = package
        .and_then(|value| value.get("packageManager"))
        .and_then(Value::as_str);
    let pnpm_lock = root.join("pnpm-lock.yaml").exists();
    let yarn_lock = root.join("yarn.lock").exists();
    let npm_lock = root.join("package-lock.json").exists();
    let npm_lock_disabled = npm_package_lock_disabled(root);
    let fallback = if pnpm_lock {
        Some("pnpm")
    } else if yarn_lock {
        Some("yarn")
    } else if npm_lock {
        Some("npm")
    } else {
        None
    };
    let (name, version, source) = if let Some(declared) = declared {
        let (name, version) = declared
            .split_once('@')
            .map(|(name, version)| (name, Some(version)))
            .unwrap_or((declared, None));
        (Some(name), version, "package.json#packageManager")
    } else {
        (fallback, None, "lockfile")
    };
    let lockfiles = [
        ("pnpm", "pnpm-lock.yaml", pnpm_lock, true),
        ("yarn", "yarn.lock", yarn_lock, true),
        ("npm", "package-lock.json", npm_lock, !npm_lock_disabled),
    ]
    .into_iter()
    .filter(|(_, _, exists, _)| *exists)
    .map(|(manager, path, _, active)| json!({"manager": manager, "path": path, "active": active}))
    .collect::<Vec<_>>();
    let active_lockfiles = lockfiles
        .iter()
        .filter(|lockfile| lockfile["active"] == true)
        .count();
    json!({
        "name": name,
        "version": version,
        "source": source,
        "lockfiles": lockfiles,
        "conflicting_lockfiles": active_lockfiles > 1,
        "npm_package_lock_disabled": npm_lock_disabled
    })
}

fn npm_package_lock_disabled(root: &Path) -> bool {
    std::fs::read_to_string(root.join(".npmrc"))
        .ok()
        .is_some_and(|content| {
            content.lines().any(|line| {
                let normalized = line.trim().to_ascii_lowercase().replace(' ', "");
                normalized == "package-lock=false"
            })
        })
}

fn package_manager_probe(declared: &Value, pnpm: &Value, corepack_pnpm: &Value) -> bool {
    match declared.get("name").and_then(Value::as_str) {
        Some("pnpm") => pnpm["healthy"] == true || corepack_pnpm["healthy"] == true,
        Some("npm") | None => true,
        Some("yarn") => false,
        Some(_) => false,
    }
}

fn probe_command(program: &str, resolved: Option<&Path>, args: &[&str]) -> Command {
    let executable = resolved.unwrap_or_else(|| Path::new(program));
    #[cfg(windows)]
    {
        let extension = executable
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(extension.as_deref(), Some("cmd" | "bat")) {
            let mut command = Command::new("cmd.exe");
            command.args(["/d", "/s", "/c"]);
            command
                .as_std_mut()
                .raw_arg(windows_batch_command_line(executable, args));
            return command;
        }
    }
    let mut command = Command::new(executable);
    command.args(args);
    #[cfg(windows)]
    crate::tools::exec::configure_windows_command_environment(
        &mut command,
        &executable.to_string_lossy(),
    );
    command
}

#[cfg(windows)]
fn windows_batch_command_line(program: &Path, args: &[&str]) -> String {
    let display = program.to_string_lossy();
    let display = display.strip_prefix("\\\\?\\").unwrap_or(&display);
    let mut command_line = format!("call \"{}\"", display.replace('"', "\"\""));
    for arg in args {
        command_line.push(' ');
        command_line.push('"');
        command_line.push_str(&arg.replace('"', "\"\""));
        command_line.push('"');
    }
    command_line
}

#[cfg(windows)]
fn redirection_trust_diagnostics() -> Value {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessMitigationPolicy(
            process: isize,
            policy: u32,
            buffer: *mut c_void,
            length: usize,
        ) -> i32;
    }

    const PROCESS_REDIRECTION_TRUST_POLICY: u32 = 16;
    let mut flags = 0u32;
    let ok = unsafe {
        GetProcessMitigationPolicy(
            GetCurrentProcess(),
            PROCESS_REDIRECTION_TRUST_POLICY,
            (&mut flags as *mut u32).cast(),
            std::mem::size_of::<u32>(),
        ) != 0
    };
    json!({
        "supported": ok,
        "enforced": ok && flags & 0x1 != 0,
        "audit": ok && flags & 0x2 != 0,
        "flags": flags,
        "error": if ok { Value::Null } else { Value::String(std::io::Error::last_os_error().to_string()) }
    })
}

#[cfg(not(windows))]
fn redirection_trust_diagnostics() -> Value {
    json!({
        "supported": false,
        "enforced": false,
        "audit": false,
        "flags": 0,
        "error": null
    })
}

fn node_modules_diagnostics(
    root: &Path,
    package: Option<&Value>,
    redirection_trust: &Value,
) -> Value {
    let node_modules = root.join("node_modules");
    let exists = node_modules.exists();
    let traversal = if exists {
        std::fs::read_dir(&node_modules)
            .map(|mut entries| entries.next().transpose().is_ok())
            .unwrap_or(false)
    } else {
        false
    };
    let canonical_target = node_modules
        .canonicalize()
        .ok()
        .map(|path| path.display().to_string());
    let direct_packages = declared_dependency_packages(package);
    let mut package_health = BTreeMap::new();
    for package_name in direct_packages {
        let package_json = node_modules.join(&package_name).join("package.json");
        let metadata = std::fs::metadata(&package_json);
        package_health.insert(
            package_name,
            json!({
                "path": package_json.display().to_string(),
                "healthy": metadata.is_ok(),
                "error": metadata.err().map(|error| error.to_string())
            }),
        );
    }
    let direct_packages_healthy = package_health
        .values()
        .all(|value| value["healthy"] == true);
    let required_bins = required_frontend_bins(package);
    let bin_dir = node_modules.join(".bin");
    let mut bins = BTreeMap::new();
    for bin in required_bins {
        let candidates = executable_candidates(&bin_dir, &bin);
        let found = candidates.iter().find(|path| path.exists()).cloned();
        let execution = found
            .as_ref()
            .map(|path| probe_path(path, &["--version"], root, Duration::from_secs(5)))
            .unwrap_or_else(|| unavailable_probe("local package binary is unavailable"));
        bins.insert(
            bin,
            json!({
                "found": found.as_ref().map(|path| path.display().to_string()),
                "healthy": execution["healthy"],
                "candidates": candidates.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
                "probe": execution
            }),
        );
    }
    let required_bins_healthy = bins.values().all(|value| value["healthy"] == true);
    let npm_metadata = node_modules.join(".package-lock.json").exists();
    let pnpm_metadata = node_modules.join(".modules.yaml").exists();
    let npm_metadata_active = npm_metadata && !npm_package_lock_disabled(root);
    let installed_node_linker = read_yaml_string(&node_modules.join(".modules.yaml"), "nodeLinker");
    let configured_node_linker = read_yaml_string(&root.join("pnpm-workspace.yaml"), "nodeLinker");
    let effective_node_linker = configured_node_linker
        .clone()
        .or_else(|| installed_node_linker.clone());
    let redirection_guard_incompatible_layout = cfg!(windows)
        && redirection_trust["enforced"] == true
        && effective_node_linker.as_deref() == Some("isolated");
    json!({
        "exists": exists,
        "traversable": traversal,
        "canonical_target": canonical_target,
        "bin_directory": bin_dir.display().to_string(),
        "direct_packages": package_health,
        "direct_packages_healthy": direct_packages_healthy,
        "required_bins": bins,
        "required_bins_healthy": required_bins_healthy,
        "npm_metadata": npm_metadata,
        "npm_metadata_active": npm_metadata_active,
        "pnpm_metadata": pnpm_metadata,
        "mixed_installer_metadata": npm_metadata_active && pnpm_metadata,
        "configured_node_linker": configured_node_linker,
        "installed_node_linker": installed_node_linker,
        "redirection_guard_incompatible_layout": redirection_guard_incompatible_layout
    })
}

fn declared_dependency_packages(package: Option<&Value>) -> Vec<String> {
    ["dependencies", "devDependencies", "optionalDependencies"]
        .into_iter()
        .filter_map(|key| {
            package
                .and_then(|value| value.get(key))
                .and_then(Value::as_object)
        })
        .flat_map(|object| object.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn read_yaml_string(path: &Path, key: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: Value = serde_yaml::from_slice(&bytes).ok()?;
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn required_frontend_bins(package: Option<&Value>) -> Vec<String> {
    let mut bins = Vec::new();
    let dependencies = ["dependencies", "devDependencies"]
        .into_iter()
        .filter_map(|key| {
            package
                .and_then(|value| value.get(key))
                .and_then(Value::as_object)
        })
        .flat_map(|object| object.keys().map(String::as_str))
        .collect::<Vec<_>>();
    if dependencies.contains(&"vite") {
        bins.push("vite".into());
    }
    if dependencies.contains(&"typescript") {
        bins.push("tsc".into());
    }
    if dependencies.contains(&"eslint") {
        bins.push("eslint".into());
    }
    bins
}

fn executable_candidates(bin_dir: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![
            bin_dir.join(format!("{name}.cmd")),
            bin_dir.join(format!("{name}.exe")),
            bin_dir.join(format!("{name}.ps1")),
        ]
    } else {
        vec![bin_dir.join(name)]
    }
}

fn has_docker_project(root: &Path) -> bool {
    [
        "Dockerfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ]
    .iter()
    .any(|path| root.join(path).exists())
}

fn unavailable_probe(message: &str) -> Value {
    json!({
        "found": false,
        "healthy": false,
        "version": null,
        "path": null,
        "error": message,
        "timed_out": false
    })
}

fn probe(program: &str, args: &[&str], cwd: &Path, limit: Duration) -> Value {
    let resolved = which::which(program)
        .ok()
        .map(|path| crate::tools::exec::resolve_system_program_path(&path));
    probe_resolved(program, resolved, args, cwd, limit)
}

fn probe_path(path: &Path, args: &[&str], cwd: &Path, limit: Duration) -> Value {
    probe_resolved(
        &path.display().to_string(),
        Some(path.to_path_buf()),
        args,
        cwd,
        limit,
    )
}

fn probe_resolved(
    program: &str,
    resolved: Option<PathBuf>,
    args: &[&str],
    cwd: &Path,
    limit: Duration,
) -> Value {
    let output = crate::async_runtime::block_on(async {
        let mut command = probe_command(program, resolved.as_deref(), args);
        crate::platform::hide_tokio_console(&mut command);
        command
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        timeout(limit, command.output()).await
    });
    match output {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            json!({
                "found": resolved.is_some(),
                "healthy": output.status.success(),
                "version": if stdout.is_empty() { Value::Null } else { Value::String(stdout) },
                "path": resolved.map(|path| path.display().to_string()),
                "error": if output.status.success() || stderr.is_empty() { Value::Null } else { Value::String(stderr) },
                "exit_code": output.status.code(),
                "timed_out": false
            })
        }
        Ok(Err(error)) => json!({
            "found": resolved.is_some(),
            "healthy": false,
            "version": null,
            "path": resolved.map(|path| path.display().to_string()),
            "error": error.to_string(),
            "exit_code": null,
            "timed_out": false
        }),
        Err(_) => json!({
            "found": resolved.is_some(),
            "healthy": false,
            "version": null,
            "path": resolved.map(|path| path.display().to_string()),
            "error": format!("probe timed out after {} ms", limit.as_millis()),
            "exit_code": null,
            "timed_out": true
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manager_uses_declared_version() {
        let package = json!({"packageManager": "pnpm@10.14.0"});
        let declared = declared_package_manager(Path::new("."), Some(&package));
        assert_eq!(declared["name"], "pnpm");
        assert_eq!(declared["version"], "10.14.0");
        assert_eq!(declared["source"], "package.json#packageManager");
    }

    #[test]
    fn required_bins_follow_declared_dependencies() {
        let package = json!({
            "devDependencies": {"vite": "1", "typescript": "1", "eslint": "1"}
        });
        assert_eq!(
            required_frontend_bins(Some(&package)),
            vec!["vite", "tsc", "eslint"]
        );
    }

    #[test]
    fn disabled_npm_lockfile_does_not_conflict_with_declared_pnpm() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(
            root.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("pnpm lock");
        std::fs::write(root.path().join("package-lock.json"), "{}\n").expect("npm lock");
        std::fs::write(root.path().join(".npmrc"), "package-lock=false\n").expect("npmrc");
        let package = json!({"packageManager": "pnpm@11.18.0"});

        let declared = declared_package_manager(root.path(), Some(&package));

        assert_eq!(declared["name"], "pnpm");
        assert_eq!(declared["conflicting_lockfiles"], false);
        assert_eq!(declared["npm_package_lock_disabled"], true);
    }

    #[test]
    fn declared_packages_are_sorted_and_deduplicated() {
        let package = json!({
            "dependencies": {"vite": "1", "shared": "1"},
            "devDependencies": {"typescript": "1", "shared": "2"}
        });

        assert_eq!(
            declared_dependency_packages(Some(&package)),
            vec!["shared", "typescript", "vite"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn probe_executes_windows_command_wrappers_through_cmd() {
        let root = tempfile::tempdir().expect("root");
        let wrapper = root.path().join("package-manager.cmd");
        std::fs::write(&wrapper, "@echo off\r\necho wrapper-ok\r\n").expect("wrapper");

        let result = probe_path(
            &wrapper,
            &["--version"],
            root.path(),
            Duration::from_secs(5),
        );

        assert_eq!(result["healthy"], true, "{result}");
        assert_eq!(result["version"], "wrapper-ok");

        let extended = PathBuf::from(format!(r"\\?\{}", wrapper.display()));
        let extended_result = probe_path(
            &extended,
            &["--version"],
            root.path(),
            Duration::from_secs(5),
        );
        assert_eq!(extended_result["healthy"], true, "{extended_result}");
        assert_eq!(extended_result["version"], "wrapper-ok");
    }

    #[cfg(windows)]
    #[test]
    fn cargo_component_probes_use_the_resolved_toolchain_environment() {
        let root = tempfile::tempdir().expect("root");
        let clippy = probe(
            "cargo",
            &["clippy", "--version"],
            root.path(),
            Duration::from_secs(10),
        );
        let fmt = probe(
            "cargo",
            &["fmt", "--version"],
            root.path(),
            Duration::from_secs(10),
        );

        assert_eq!(clippy["healthy"], true, "{clippy}");
        assert_eq!(fmt["healthy"], true, "{fmt}");
    }
}
