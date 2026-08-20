use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceService {
    Mcp,
    Actions,
}

impl WorkspaceService {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "MCP",
            Self::Actions => "Actions",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ServiceClaim<'a> {
    profile: &'a WorkspaceProfile,
    service: WorkspaceService,
    local_port: u16,
    subdomain: &'a str,
    uses_frp: bool,
}

/// Assign free MCP / Actions ports for a newly created workspace.
///
/// Creating a workspace should not force the user to edit ports first. Defaults
/// are kept when free; otherwise the next free ports above the defaults are used.
pub fn assign_free_workspace_ports_with_reserved(
    profiles: &[WorkspaceProfile],
    candidate: &mut WorkspaceProfile,
    additional_reserved: &std::collections::HashSet<u16>,
) -> AppResult<()> {
    let mut reserved: std::collections::HashSet<u16> = profiles
        .iter()
        .filter(|profile| profile.id != candidate.id)
        .flat_map(service_claims)
        .map(|claim| claim.local_port)
        .collect();
    reserved.extend(additional_reserved.iter().copied());

    let mcp_port = next_free_port(candidate.runtime.local_port, &reserved)?;
    let mut reserved_with_mcp = reserved;
    reserved_with_mcp.insert(mcp_port);
    let actions_port = next_free_port(candidate.actions.local_port, &reserved_with_mcp)?;

    candidate.runtime.local_port = mcp_port;
    candidate.actions.local_port = actions_port;
    Ok(())
}

fn next_free_port(preferred: u16, reserved: &std::collections::HashSet<u16>) -> AppResult<u16> {
    let start = if preferred == 0 { 1 } else { preferred };
    for port in start..=u16::MAX {
        if reserved.contains(&port) {
            continue;
        }
        return Ok(port);
    }
    Err(AppError::Message(format!(
        "无法从端口 {preferred} 起找到可用本地端口"
    )))
}

/// Validate an update without blocking a repair because another, unchanged
/// service already has an unchanged duplicate resource.
pub fn validate_workspace_resources_update(
    profiles: &[WorkspaceProfile],
    current: &WorkspaceProfile,
    candidate: &WorkspaceProfile,
) -> AppResult<()> {
    let existing_claims: Vec<_> = profiles
        .iter()
        .filter(|profile| profile.id != candidate.id)
        .flat_map(service_claims)
        .collect();
    let candidate_claims = service_claims(candidate);

    validate_changed_candidate_ports(&existing_claims, &candidate_claims, current, candidate)?;
    validate_changed_candidate_subdomains(&existing_claims, &candidate_claims, current, candidate)
}

#[cfg(any(feature = "cli", test))]
pub fn validate_service_start(
    profiles: &[WorkspaceProfile],
    workspace_id: &str,
    service: WorkspaceService,
) -> AppResult<()> {
    let target = profiles
        .iter()
        .find(|profile| profile.id == workspace_id)
        .ok_or_else(|| AppError::Message(format!("workspace not found: {workspace_id}")))?;
    let target_claim = claim_for(target, service);

    for profile in profiles {
        for claim in service_claims(profile) {
            if claim.profile.id == workspace_id && claim.service == service {
                continue;
            }
            if claim.local_port == target_claim.local_port {
                return Err(port_conflict_error(target_claim, claim));
            }
            if same_non_empty_subdomain(target_claim, claim) {
                return Err(subdomain_conflict_error(target_claim, claim));
            }
        }
    }

    Ok(())
}

fn validate_changed_candidate_ports(
    existing: &[ServiceClaim<'_>],
    candidate: &[ServiceClaim<'_>],
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
) -> AppResult<()> {
    for claim in candidate
        .iter()
        .copied()
        .filter(|claim| service_changed(current, next, claim.service))
    {
        if let Some(owner) = existing
            .iter()
            .copied()
            .find(|owner| owner.local_port == claim.local_port)
        {
            return Err(port_conflict_error(claim, owner));
        }
        if let Some(owner) = candidate
            .iter()
            .copied()
            .find(|owner| owner.service != claim.service && owner.local_port == claim.local_port)
        {
            return Err(port_conflict_error(claim, owner));
        }
    }
    Ok(())
}

fn validate_changed_candidate_subdomains(
    existing: &[ServiceClaim<'_>],
    candidate: &[ServiceClaim<'_>],
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
) -> AppResult<()> {
    for claim in candidate
        .iter()
        .copied()
        .filter(|claim| service_changed(current, next, claim.service) && claim.uses_frp)
    {
        if claim.subdomain.trim().is_empty() {
            continue;
        }
        if let Some(owner) = existing
            .iter()
            .copied()
            .find(|owner| same_non_empty_subdomain(claim, *owner))
        {
            return Err(subdomain_conflict_error(claim, owner));
        }
        if let Some(owner) = candidate
            .iter()
            .copied()
            .find(|owner| owner.service != claim.service && same_non_empty_subdomain(claim, *owner))
        {
            return Err(subdomain_conflict_error(claim, owner));
        }
    }
    Ok(())
}

fn service_claims(profile: &WorkspaceProfile) -> [ServiceClaim<'_>; 2] {
    [
        claim_for(profile, WorkspaceService::Mcp),
        claim_for(profile, WorkspaceService::Actions),
    ]
}

fn service_changed(
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
    service: WorkspaceService,
) -> bool {
    let current = claim_for(current, service);
    let next = claim_for(next, service);
    current.local_port != next.local_port
        || current.subdomain != next.subdomain
        || current.uses_frp != next.uses_frp
}

fn claim_for(profile: &WorkspaceProfile, service: WorkspaceService) -> ServiceClaim<'_> {
    match service {
        WorkspaceService::Mcp => ServiceClaim {
            profile,
            service,
            local_port: profile.runtime.local_port,
            subdomain: profile.tunnel.frp_subdomain.as_str(),
            uses_frp: profile.tunnel.tunnel_type == "frp",
        },
        WorkspaceService::Actions => ServiceClaim {
            profile,
            service,
            local_port: profile.actions.local_port,
            subdomain: profile.actions.frp_subdomain.as_str(),
            uses_frp: profile.actions.tunnel_type == "frp",
        },
    }
}

fn same_non_empty_subdomain(left: ServiceClaim<'_>, right: ServiceClaim<'_>) -> bool {
    if !left.uses_frp || !right.uses_frp {
        return false;
    }
    let left = left.subdomain.trim();
    let right = right.subdomain.trim();
    !left.is_empty() && !right.is_empty() && left.eq_ignore_ascii_case(right)
}

fn port_conflict_error(target: ServiceClaim<'_>, owner: ServiceClaim<'_>) -> AppError {
    AppError::Message(format!(
        "本地端口 {} 与工作区“{}”的 {} 服务重复。请修改当前工作区 {} 端口后再启动。",
        target.local_port,
        owner.profile.name,
        owner.service.label(),
        target.service.label()
    ))
}

fn subdomain_conflict_error(target: ServiceClaim<'_>, owner: ServiceClaim<'_>) -> AppError {
    AppError::Message(format!(
        "FRP 子域名“{}”已被工作区“{}”的 {} 服务使用，当前工作区 {} 不能启动。",
        target.subdomain.trim(),
        owner.profile.name,
        owner.service.label(),
        target.service.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        assign_free_workspace_ports_with_reserved, validate_service_start,
        validate_workspace_resources_update, WorkspaceService,
    };
    use crate::workspace::WorkspaceProfile;

    fn profile(name: &str, mcp_port: u16, actions_port: u16) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(format!("C:/workspace/{name}"), Some(name.into()));
        profile.runtime.local_port = mcp_port;
        profile.actions.local_port = actions_port;
        profile.tunnel.tunnel_type = "frp".into();
        profile.tunnel.frp_subdomain = format!("{name}-mcp");
        profile.actions.tunnel_type = "frp".into();
        profile.actions.frp_subdomain = format!("{name}-actions");
        profile
    }

    #[test]
    fn rejects_duplicate_mcp_port_across_workspaces_before_start() {
        let owner = profile("owner", 28_766, 8_787);
        let target = profile("target", 28_766, 8_788);

        let error = validate_service_start(
            &[owner.clone(), target.clone()],
            &target.id,
            WorkspaceService::Mcp,
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("28766"));
        assert!(message.contains(&owner.name));
        assert!(message.contains("MCP"));
    }

    #[test]
    fn assign_free_ports_keeps_defaults_when_available() {
        let mut candidate = WorkspaceProfile::new("C:/workspace/new".into(), Some("new".into()));

        assign_free_workspace_ports_with_reserved(
            &[],
            &mut candidate,
            &std::collections::HashSet::new(),
        )
        .expect("assign");

        assert_eq!(candidate.runtime.local_port, 28_766);
        assert_eq!(candidate.actions.local_port, 8_787);
    }

    #[test]
    fn assign_free_ports_skips_ports_claimed_by_other_workspaces() {
        let owner = profile("owner", 28_766, 8_787);
        let mut candidate = WorkspaceProfile::new("C:/workspace/new".into(), Some("new".into()));

        assign_free_workspace_ports_with_reserved(
            std::slice::from_ref(&owner),
            &mut candidate,
            &std::collections::HashSet::new(),
        )
        .expect("assign");

        assert_eq!(candidate.runtime.local_port, 28_767);
        assert_eq!(candidate.actions.local_port, 8_788);
        assert!(validate_service_start(
            &[owner, candidate.clone()],
            &candidate.id,
            WorkspaceService::Mcp,
        )
        .is_ok());
    }

    #[test]
    fn assign_free_ports_skips_gateway_reserved_port() {
        let mut candidate = WorkspaceProfile::new("C:/workspace/new".into(), Some("new".into()));
        candidate.runtime.local_port = 28_765;
        let reserved = std::collections::HashSet::from([28_765]);

        assign_free_workspace_ports_with_reserved(&[], &mut candidate, &reserved).expect("assign");

        assert_eq!(candidate.runtime.local_port, 28_766);
    }

    #[test]
    fn assign_free_ports_avoids_colliding_mcp_and_actions() {
        // MCP default is free, but preferred actions port is already claimed as MCP.
        let owner = profile("owner", 8_787, 9_001);
        let mut candidate = WorkspaceProfile::new("C:/workspace/new".into(), Some("new".into()));

        assign_free_workspace_ports_with_reserved(
            std::slice::from_ref(&owner),
            &mut candidate,
            &std::collections::HashSet::new(),
        )
        .expect("assign");

        assert_eq!(candidate.runtime.local_port, 28_766);
        assert_eq!(candidate.actions.local_port, 8_788);
        assert_ne!(candidate.runtime.local_port, candidate.actions.local_port);
    }

    #[test]
    fn rejects_port_shared_by_mcp_and_another_workspace_actions() {
        let mut owner = profile("owner", 28_765, 8_787);
        owner.actions.local_port = 28_766;
        let target = profile("target", 28_766, 8_788);

        let error = validate_service_start(
            &[owner.clone(), target.clone()],
            &target.id,
            WorkspaceService::Mcp,
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("28766"));
        assert!(message.contains(&owner.name));
        assert!(message.contains("Actions"));
    }

    #[test]
    fn rejects_duplicate_ports_between_services_in_one_workspace() {
        let candidate = profile("target", 28_766, 28_766);

        let error = validate_service_start(
            std::slice::from_ref(&candidate),
            &candidate.id,
            WorkspaceService::Mcp,
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("28766"));
        assert!(message.contains("MCP"));
        assert!(message.contains("Actions"));
    }

    #[test]
    fn allows_a_workspace_to_keep_its_own_ports() {
        let original = profile("target", 28_766, 8_787);
        let updated = original.clone();

        assert!(validate_workspace_resources_update(
            std::slice::from_ref(&original),
            &original,
            &updated,
        )
        .is_ok());
    }

    #[test]
    fn allows_fixing_mcp_port_when_actions_conflict_is_unchanged() {
        let owner = profile("owner", 28_765, 8_787);
        let current = profile("target", 28_766, 8_787);
        let mut candidate = current.clone();
        candidate.runtime.local_port = 28_767;

        assert!(validate_workspace_resources_update(
            &[owner, current.clone()],
            &current,
            &candidate,
        )
        .is_ok());
    }

    #[test]
    fn rejects_changed_mcp_port_that_conflicts_with_another_service() {
        let owner = profile("owner", 28_765, 8_787);
        let current = profile("target", 28_766, 8_788);
        let mut candidate = current.clone();
        candidate.runtime.local_port = owner.runtime.local_port;

        let error =
            validate_workspace_resources_update(&[owner, current.clone()], &current, &candidate)
                .unwrap_err();

        assert!(error.to_string().contains("28765"));
    }

    #[test]
    fn start_is_blocked_when_target_participates_in_existing_duplicate() {
        let first = profile("first", 28_766, 8_787);
        let target = profile("target", 28_766, 8_788);

        let error =
            validate_service_start(&[first, target.clone()], &target.id, WorkspaceService::Mcp)
                .unwrap_err();

        assert!(error.to_string().contains("端口"));
    }

    #[test]
    fn rejects_duplicate_subdomains_with_owner_details() {
        let owner = profile("owner", 28_765, 8_787);
        let mut candidate = profile("target", 28_766, 8_788);
        candidate.tunnel.frp_subdomain = owner.tunnel.frp_subdomain.clone();

        let error = validate_service_start(
            &[owner.clone(), candidate.clone()],
            &candidate.id,
            WorkspaceService::Mcp,
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains(&owner.tunnel.frp_subdomain));
        assert!(message.contains(&owner.name));
        assert!(message.contains("MCP"));
    }

    #[test]
    fn ignores_blank_subdomain_claims() {
        let mut first = profile("first", 28_765, 8_787);
        let mut second = profile("second", 28_766, 8_788);
        first.tunnel.frp_subdomain.clear();
        second.tunnel.frp_subdomain.clear();

        assert!(validate_service_start(
            &[first, second.clone()],
            &second.id,
            WorkspaceService::Mcp,
        )
        .is_ok());
    }
}
