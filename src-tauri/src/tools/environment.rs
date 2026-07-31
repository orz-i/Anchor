use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;

pub fn diagnose(root: &Path) -> Value {
    let package = read_package_json(root);
    let declared = declared_package_manager(root, package.as_ref());
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
    let node_modules = node_modules_diagnostics(root, package.as_ref());
    let host_frontend_healthy = node["healthy"] == true
        && package_manager_probe(&declared, &pnpm, &corepack_pnpm)
        && node_modules["traversable"] == true
        && node_modules["required_bins_healthy"] == true;
    let docker_project = has_docker_project(root);
    let recommended_route = if host_frontend_healthy {
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
    if cargo["healthy"] != true && rustup_cargo["healthy"] == true {
        findings.push(
            "The cargo proxy is unhealthy, but `rustup run stable cargo` is available".to_string(),
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
    if recommended_route == "docker" {
        findings.push("Docker frontend verification is healthy and preferred".to_string());
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
        "package_manager": declared,
        "probes": {
            "node": node,
            "pnpm": pnpm,
            "corepack": corepack,
            "corepack_pnpm": corepack_pnpm,
            "cargo": cargo,
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
        "recommended_verification_route": recommended_route,
        "findings": findings
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
    let fallback = if root.join("pnpm-lock.yaml").exists() {
        Some("pnpm")
    } else if root.join("yarn.lock").exists() {
        Some("yarn")
    } else if root.join("package-lock.json").exists() {
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
    json!({
        "name": name,
        "version": version,
        "source": source
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

fn node_modules_diagnostics(root: &Path, package: Option<&Value>) -> Value {
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
    let required_bins = required_frontend_bins(package);
    let bin_dir = node_modules.join(".bin");
    let mut bins = BTreeMap::new();
    for bin in required_bins {
        let candidates = executable_candidates(&bin_dir, &bin);
        let found = candidates.iter().find(|path| path.exists()).cloned();
        let traversable = found
            .as_ref()
            .is_some_and(|path| std::fs::metadata(path).is_ok());
        bins.insert(
            bin,
            json!({
                "found": found.as_ref().map(|path| path.display().to_string()),
                "healthy": traversable,
                "candidates": candidates.iter().map(|path| path.display().to_string()).collect::<Vec<_>>()
            }),
        );
    }
    let required_bins_healthy = bins.values().all(|value| value["healthy"] == true);
    json!({
        "exists": exists,
        "traversable": traversal,
        "canonical_target": canonical_target,
        "bin_directory": bin_dir.display().to_string(),
        "required_bins": bins,
        "required_bins_healthy": required_bins_healthy
    })
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
    let resolved = which::which(program).ok();
    let output = crate::async_runtime::block_on(async {
        let mut command = Command::new(program);
        crate::platform::hide_tokio_console(&mut command);
        command
            .args(args)
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
}
