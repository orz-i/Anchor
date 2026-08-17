use std::collections::HashSet;
use std::time::Duration;

use serde::Serialize;

use super::args::UpgradeOptions;
use crate::build_identity::BuildIdentity;
use crate::daemon;
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::gateway_daemon;
use crate::rollout::{self, RolloutMode, RolloutOptions, RuntimeRolloutResult};
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpgradeReport {
    event: &'static str,
    dry_run: bool,
    current_build: BuildIdentity,
    results: Vec<RuntimeRolloutResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub async fn execute(options: UpgradeOptions, as_json: bool) -> AppResult<i32> {
    if !daemon::supported() || !gateway_daemon::supported() {
        return Err(AppError::Message(
            "runtime upgrade 当前仅支持 Windows 和 Linux daemon 模式".into(),
        ));
    }
    let store = DataStore::load()?;
    let profiles = store.list().to_vec();
    drop(store);
    let targets = select_workspace_targets(&profiles, &options)?;
    let include_gateway = select_gateway_target(&options)?;
    ensure_windows_scm_does_not_own_targets(&targets, include_gateway)?;

    let rollout_options = RolloutOptions {
        timeout: Duration::from_secs(options.timeout_seconds),
        force: options.force,
        dry_run: true,
        allow_no_rollback: options.allow_no_rollback,
    };
    let mut preflight = Vec::new();
    for profile in &targets {
        preflight.push(rollout::rollout_workspace(profile, rollout_options).await?);
    }
    if include_gateway {
        preflight.push(rollout::rollout_gateway(rollout_options).await?);
    }
    if options.dry_run {
        let report = UpgradeReport {
            event: "runtime_upgrade_plan",
            dry_run: true,
            current_build: BuildIdentity::current(),
            results: preflight,
            error: None,
        };
        print_report(&report, as_json)?;
        return Ok(0);
    }

    let rollout_options = RolloutOptions {
        dry_run: false,
        ..rollout_options
    };
    let mut results = Vec::new();
    let mut error = None;
    for profile in &targets {
        match rollout::rollout_workspace(profile, rollout_options).await {
            Ok(result) => {
                let continue_rollout = result.is_success();
                results.push(result);
                if !continue_rollout {
                    break;
                }
            }
            Err(failure) => {
                error = Some(format!("Workspace {}: {failure}", profile.name));
                break;
            }
        }
    }
    if error.is_none() && results.iter().all(RuntimeRolloutResult::is_success) && include_gateway {
        match rollout::rollout_gateway(rollout_options).await {
            Ok(result) => results.push(result),
            Err(failure) => error = Some(format!("Gateway: {failure}")),
        }
    }

    let successful = error.is_none() && results.iter().all(RuntimeRolloutResult::is_success);
    let report = UpgradeReport {
        event: "runtime_upgrade_complete",
        dry_run: false,
        current_build: BuildIdentity::current(),
        results,
        error,
    };
    print_report(&report, as_json)?;
    Ok(if successful { 0 } else { 1 })
}

fn select_workspace_targets(
    profiles: &[WorkspaceProfile],
    options: &UpgradeOptions,
) -> AppResult<Vec<WorkspaceProfile>> {
    if options.all {
        let mut selected = Vec::new();
        for profile in profiles {
            let inspection = daemon::inspect(profile)?;
            if inspection.ambiguous {
                return Err(AppError::Message(inspection.detail));
            }
            if inspection.running && inspection.pid_matches {
                selected.push(profile.clone());
            }
        }
        return Ok(selected);
    }

    let mut ids = HashSet::new();
    let mut selected = Vec::new();
    for selector in &options.workspaces {
        let profile = super::resolve_workspace(profiles, selector)?;
        if ids.insert(profile.id.clone()) {
            selected.push(profile.clone());
        }
    }
    Ok(selected)
}

fn select_gateway_target(options: &UpgradeOptions) -> AppResult<bool> {
    if options.gateway {
        return Ok(true);
    }
    if !options.all {
        return Ok(false);
    }
    let inspection = gateway_daemon::inspect()?;
    if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }
    Ok(inspection.running && inspection.pid_matches)
}

#[cfg(windows)]
fn ensure_windows_scm_does_not_own_targets(
    targets: &[WorkspaceProfile],
    include_gateway: bool,
) -> AppResult<()> {
    let status = crate::windows_service::scm_status()?;
    if !status.installed || status.state == "stopped" || status.state == "not_installed" {
        return Ok(());
    }
    let selected = targets
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<HashSet<_>>();
    let workspace_owned = status
        .plan
        .workspaces
        .iter()
        .any(|entry| selected.contains(entry.workspace_id.as_str()));
    let gateway_owned = include_gateway && !status.plan.gateway_workspace_ids.is_empty();
    if !workspace_owned && !gateway_owned {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "Windows SCM service {} 正在管理所选 runtime；普通 CLI 不会与 supervisor 竞争拉起。请先以管理员权限运行 `anchor service install` 将 SCM 更新到当前构建，由 Service 排空并恢复其 desired state，然后重新运行 `anchor upgrade --dry-run ...` 验证 build identity",
        status.service_name
    )))
}

#[cfg(not(windows))]
fn ensure_windows_scm_does_not_own_targets(
    _targets: &[WorkspaceProfile],
    _include_gateway: bool,
) -> AppResult<()> {
    Ok(())
}

fn print_report(report: &UpgradeReport, as_json: bool) -> AppResult<()> {
    if as_json {
        return super::print_json(report);
    }
    println!(
        "Anchor runtime upgrade: package={} git={}{}{}",
        report.current_build.package_version,
        report.current_build.short_git_sha(),
        if report.current_build.git_dirty {
            " dirty"
        } else {
            ""
        },
        if report.dry_run { " (dry-run)" } else { "" }
    );
    if report.results.is_empty() {
        println!("没有匹配的运行中 runtime。");
    }
    for result in &report.results {
        let target = match result.workspace_name.as_deref() {
            Some(name) => format!("Workspace {name}"),
            None => "Gateway".into(),
        };
        let mut details = Vec::new();
        if let Some(previous_pid) = result.previous_pid {
            details.push(format!("old-pid={previous_pid}"));
        }
        if let Some(pid) = result.pid {
            details.push(format!("pid={pid}"));
        }
        if let Some(mode) = result.mode {
            details.push(format!(
                "mode={}",
                match mode {
                    RolloutMode::ZeroDowntimeHandoff => "zero-downtime",
                    RolloutMode::BoundedOutage => "bounded-outage",
                }
            ));
        }
        if let Some(outage_ms) = result.outage_ms {
            details.push(format!("outage={}ms", outage_ms));
        }
        if let Some(listener_ready_ms) = result.listener_ready_ms {
            details.push(format!("listener-ready={}ms", listener_ready_ms));
        }
        if let Some(drain_ms) = result.drain_ms {
            details.push(format!("drain={}ms", drain_ms));
        }
        if result.rollback_attempted {
            details.push(format!(
                "rollback={}",
                if result.rollback_succeeded == Some(true) {
                    "succeeded"
                } else {
                    "failed"
                }
            ));
        } else if result.rollback_available {
            details.push("rollback=available".into());
        }
        println!("{}\t{:?}\t{}", target, result.status, details.join(" "));
        if let Some(message) = result.message.as_deref() {
            println!("  {message}");
        }
        if let Some(failure) = result.failure.as_deref() {
            println!("  failure: {failure}");
        }
        if let Some(failure) = result.rollback_failure.as_deref() {
            println!("  rollback failure: {failure}");
        }
    }
    if let Some(error) = report.error.as_deref() {
        eprintln!("升级中止：{error}");
    }
    Ok(())
}
