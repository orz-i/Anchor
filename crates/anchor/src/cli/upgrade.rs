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
    #[serde(skip_serializing_if = "Option::is_none")]
    supervisor: Option<SupervisorUpgradeReport>,
    results: Vec<RuntimeRolloutResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn supervisor_plan_report(route: &SupervisorRoute) -> SupervisorUpgradeReport {
    let mut workspace_ids = route.workspace_ids.iter().cloned().collect::<Vec<_>>();
    workspace_ids.sort();
    SupervisorUpgradeReport {
        manager: route.manager.clone(),
        status: "planned",
        workspace_ids,
        gateway_managed: route.gateway_managed,
        previous_build: route.previous_build.clone(),
        current_build: BuildIdentity::current(),
        message: "Supervisor owns the selected runtime; upgrade will refresh the supervisor executable/build plan and let it reconcile desired runtime state instead of racing a direct daemon rollout."
            .into(),
    }
}

fn supervisor_scope_route(
    manager: String,
    managed: HashSet<String>,
    gateway_managed: bool,
    previous_build: Option<BuildIdentity>,
    targets: &[WorkspaceProfile],
    include_gateway: bool,
    all: bool,
) -> AppResult<Option<SupervisorRoute>> {
    let selected = targets
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<HashSet<_>>();
    let overlap =
        selected.iter().any(|id| managed.contains(id)) || (include_gateway && gateway_managed);
    if !overlap {
        return Ok(None);
    }
    if !all {
        let mut missing = managed.difference(&selected).cloned().collect::<Vec<_>>();
        missing.sort();
        if !missing.is_empty() || (gateway_managed && !include_gateway) {
            return Err(AppError::Message(format!(
                "SUPERVISOR_UPGRADE_SCOPE_MISMATCH: {manager} owns a broader desired runtime set than this upgrade request (missing_workspaces={missing:?}, gateway_managed={gateway_managed}); use `anchor upgrade --all` or explicitly select every service-managed workspace{}",
                if gateway_managed { " plus --gateway" } else { "" }
            )));
        }
    }
    Ok(Some(SupervisorRoute {
        manager,
        workspace_ids: managed,
        gateway_managed,
        previous_build,
    }))
}

fn linux_supervisor_route(
    targets: &[WorkspaceProfile],
    include_gateway: bool,
    all: bool,
) -> AppResult<Option<SupervisorRoute>> {
    #[cfg(target_os = "linux")]
    {
        let status = crate::linux_service::service_status()?;
        if !status.installed || !status.enabled || !status.running {
            return Ok(None);
        }
        let managed = status
            .plan
            .workspaces
            .iter()
            .map(|entry| entry.workspace_id.clone())
            .collect::<HashSet<_>>();
        let gateway_managed = !status.plan.gateway_workspace_ids.is_empty();
        supervisor_scope_route(
            status.manager,
            managed,
            gateway_managed,
            status.plan.installed_build,
            targets,
            include_gateway,
            all,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (targets, include_gateway, all);
        Ok(None)
    }
}

fn apply_linux_supervisor_route(route: &SupervisorRoute) -> AppResult<SupervisorUpgradeReport> {
    #[cfg(target_os = "linux")]
    {
        let status = crate::linux_service::install_service()?;
        if status.build_state != "current" {
            return Err(AppError::Message(format!(
                "SUPERVISOR_UPGRADE_VERIFY_FAILED: systemd-user service restarted but buildState={} instead of current",
                status.build_state
            )));
        }
        let mut workspace_ids = route.workspace_ids.iter().cloned().collect::<Vec<_>>();
        workspace_ids.sort();
        Ok(SupervisorUpgradeReport {
            manager: route.manager.clone(),
            status: "upgraded",
            workspace_ids,
            gateway_managed: route.gateway_managed,
            previous_build: route.previous_build.clone(),
            current_build: status.current_build,
            message: "Supervisor executable/build plan is current and desired runtime state is reconciled by systemd-user."
                .into(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = route;
        Err(AppError::Message(
            "Linux supervisor upgrade is unavailable on this platform".into(),
        ))
    }
}

#[derive(Debug, Clone)]
struct SupervisorRoute {
    manager: String,
    workspace_ids: HashSet<String>,
    gateway_managed: bool,
    previous_build: Option<BuildIdentity>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupervisorUpgradeReport {
    manager: String,
    status: &'static str,
    workspace_ids: Vec<String>,
    gateway_managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_build: Option<BuildIdentity>,
    current_build: BuildIdentity,
    message: String,
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
    let supervisor_route = linux_supervisor_route(&targets, include_gateway, options.all)?;
    let direct_targets = targets
        .iter()
        .filter(|profile| {
            !supervisor_route
                .as_ref()
                .is_some_and(|route| route.workspace_ids.contains(&profile.id))
        })
        .cloned()
        .collect::<Vec<_>>();
    let direct_gateway = include_gateway
        && !supervisor_route
            .as_ref()
            .is_some_and(|route| route.gateway_managed);

    let rollout_options = RolloutOptions {
        timeout: Duration::from_secs(options.timeout_seconds),
        force: options.force,
        dry_run: true,
        allow_no_rollback: options.allow_no_rollback,
    };
    let mut preflight = Vec::new();
    for profile in &direct_targets {
        preflight.push(rollout::rollout_workspace(profile, rollout_options).await?);
    }
    if direct_gateway {
        preflight.push(rollout::rollout_gateway(rollout_options).await?);
    }
    if options.dry_run {
        let report = UpgradeReport {
            event: "runtime_upgrade_plan",
            dry_run: true,
            current_build: BuildIdentity::current(),
            supervisor: supervisor_route.as_ref().map(supervisor_plan_report),
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
    let supervisor = match supervisor_route.as_ref() {
        Some(route) => match apply_linux_supervisor_route(route) {
            Ok(report) => Some(report),
            Err(failure) => {
                error = Some(format!("{} supervisor: {failure}", route.manager));
                None
            }
        },
        None => None,
    };
    for profile in &direct_targets {
        if error.is_some() {
            break;
        }
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
    if error.is_none() && results.iter().all(RuntimeRolloutResult::is_success) && direct_gateway {
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
        supervisor,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn profile(id: &str) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(format!("/tmp/{id}"), Some(id.into()));
        profile.id = id.into();
        profile
    }

    #[test]
    fn supervisor_scope_requires_full_desired_set_for_explicit_upgrade() {
        let managed = ["a".to_string(), "b".to_string()]
            .into_iter()
            .collect::<HashSet<_>>();
        let error = supervisor_scope_route(
            "systemd-user".into(),
            managed.clone(),
            true,
            None,
            &[profile("a")],
            false,
            false,
        )
        .expect_err("partial supervisor scope must fail");
        assert!(error
            .to_string()
            .contains("SUPERVISOR_UPGRADE_SCOPE_MISMATCH"));

        let route = supervisor_scope_route(
            "systemd-user".into(),
            managed.clone(),
            true,
            None,
            &[profile("a"), profile("b")],
            true,
            false,
        )
        .expect("full explicit scope")
        .expect("supervisor route");
        assert_eq!(route.workspace_ids, managed);
        assert!(route.gateway_managed);

        let all_route = supervisor_scope_route(
            "systemd-user".into(),
            ["a".to_string(), "b".to_string()].into_iter().collect(),
            true,
            None,
            &[profile("a")],
            false,
            true,
        )
        .expect("all permits full supervisor-owned desired state")
        .expect("supervisor route");
        assert!(all_route.gateway_managed);
    }
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
    if let Some(supervisor) = report.supervisor.as_ref() {
        println!(
            "Supervisor {}\t{}\tworkspaces={} gateway={}",
            supervisor.manager,
            supervisor.status,
            supervisor.workspace_ids.join(","),
            supervisor.gateway_managed
        );
        println!("  {}", supervisor.message);
    }
    if report.results.is_empty() && report.supervisor.is_none() {
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
