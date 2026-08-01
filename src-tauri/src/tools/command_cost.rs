use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::policy::PolicySettings;
use crate::tools::workspace::WorkspaceError;

const POLICY_PATH: &str = ".anchor/command-policy.yml";

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    #[default]
    Free,
    LocalExpensive,
    ExternalPaid,
}

impl CostClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::LocalExpensive => "local_expensive",
            Self::ExternalPaid => "external_paid",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CommandPolicyFile {
    #[serde(default)]
    commands: BTreeMap<String, CommandPolicyRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CommandPolicyRule {
    #[serde(rename = "match")]
    pattern: String,
    #[serde(default)]
    cost_class: CostClass,
    #[serde(default)]
    require_confirmation: bool,
    max_runs: Option<u64>,
    max_duration_seconds: Option<u64>,
    max_retries: Option<u32>,
    max_external_calls: Option<u64>,
    max_tokens: Option<u64>,
    max_cost_usd: Option<f64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CostState {
    runs: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct CommandCostDecision {
    cost_class: CostClass,
    rule_name: Option<String>,
    requested_timeout_ms: u64,
    effective_timeout_ms: u64,
    confirmation_required: bool,
    operator_approved: bool,
    max_runs_per_day: Option<u64>,
    run_number: Option<u64>,
    max_retries: Option<u32>,
    max_external_calls: Option<u64>,
    max_tokens: Option<u64>,
    max_cost_usd: Option<f64>,
    source: &'static str,
    executable: Option<String>,
    indicators: Vec<String>,
    cost_intent: String,
    network_mode: String,
}

impl CommandCostDecision {
    pub fn effective_timeout_ms(&self) -> u64 {
        self.effective_timeout_ms
    }

    pub fn to_value(&self) -> Value {
        json!({
            "cost_class": self.cost_class.as_str(),
            "rule": self.rule_name,
            "source": self.source,
            "executable": self.executable,
            "indicators": self.indicators,
            "cost_intent": self.cost_intent,
            "network_mode": self.network_mode,
            "confirmation_required": self.confirmation_required,
            "operator_approved": self.operator_approved,
            "requested_timeout_ms": self.requested_timeout_ms,
            "effective_timeout_ms": self.effective_timeout_ms,
            "max_runs_per_day": self.max_runs_per_day,
            "run_number": self.run_number,
            "max_retries": self.max_retries,
            "max_external_calls": self.max_external_calls,
            "max_tokens": self.max_tokens,
            "max_cost_usd": self.max_cost_usd,
            "usage_enforcement": {
                "runs": self.max_runs_per_day.is_some(),
                "duration": true,
                "external_calls": "declared_not_observable",
                "tokens": "declared_not_observable",
                "cost": "declared_not_observable"
            },
            "policy_path": POLICY_PATH
        })
    }
}

pub struct CommandCostGuard {
    state_path: PathBuf,
    state: Mutex<CostState>,
}

impl CommandCostGuard {
    pub fn new(harness_root: &Path, workspace_root: &Path) -> Self {
        let workspace_key = digest(workspace_root.to_string_lossy().as_bytes());
        let state_path = harness_root
            .join("command-cost")
            .join(format!("{workspace_key}.json"));
        let state = fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            state_path,
            state: Mutex::new(state),
        }
    }

    pub fn evaluate(
        &self,
        workspace_root: &Path,
        command: &str,
        requested_timeout_ms: u64,
        cost_intent: &str,
        network_mode: &str,
        policy: &PolicySettings,
    ) -> Result<CommandCostDecision, WorkspaceError> {
        self.evaluate_internal(
            workspace_root,
            command,
            requested_timeout_ms,
            cost_intent,
            network_mode,
            policy,
            true,
        )
    }

    pub fn explain(
        &self,
        workspace_root: &Path,
        command: &str,
        requested_timeout_ms: u64,
        cost_intent: &str,
        network_mode: &str,
        policy: &PolicySettings,
    ) -> Result<Value, WorkspaceError> {
        let decision = self.evaluate_internal(
            workspace_root,
            command,
            requested_timeout_ms,
            cost_intent,
            network_mode,
            policy,
            false,
        )?;
        Ok(json!({
            "command": command,
            "classification": decision.to_value(),
            "would_require_operator_approval": decision.cost_class == CostClass::ExternalPaid
                && !decision.operator_approved,
            "executed": false,
            "run_budget_reserved": false
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_internal(
        &self,
        workspace_root: &Path,
        command: &str,
        requested_timeout_ms: u64,
        cost_intent: &str,
        network_mode: &str,
        policy: &PolicySettings,
        enforce: bool,
    ) -> Result<CommandCostDecision, WorkspaceError> {
        validate_declarations(cost_intent, network_mode)?;
        let configured = load_policy(workspace_root)?;
        let matched = match_rule(&configured, command)?;
        let heuristic = command_cost_heuristic(command, cost_intent, network_mode)?;
        let (rule_name, rule, source, indicators) = match matched {
            Some((name, rule)) => (
                Some(name),
                rule,
                "workspace_policy",
                vec!["workspace_policy_match".to_string()],
            ),
            None if heuristic.cost_class == CostClass::ExternalPaid => (
                Some("anchor_evidence_external_paid".to_string()),
                CommandPolicyRule {
                    cost_class: CostClass::ExternalPaid,
                    require_confirmation: true,
                    ..CommandPolicyRule::default()
                },
                "anchor_evidence",
                heuristic.indicators.clone(),
            ),
            None => (
                None,
                CommandPolicyRule::default(),
                "default_local",
                heuristic.indicators.clone(),
            ),
        };

        let confirmation_required =
            rule.require_confirmation || rule.cost_class == CostClass::ExternalPaid;
        let operator_approved =
            rule.cost_class != CostClass::ExternalPaid || policy.external_paid_commands_enabled;
        let global_duration_ms = policy
            .external_paid_max_duration_seconds
            .max(1)
            .saturating_mul(1000);
        let rule_duration_ms = rule
            .max_duration_seconds
            .unwrap_or(policy.external_paid_max_duration_seconds)
            .max(1)
            .saturating_mul(1000);
        let effective_timeout_ms = if rule.cost_class == CostClass::ExternalPaid {
            requested_timeout_ms.min(global_duration_ms.min(rule_duration_ms))
        } else if let Some(seconds) = rule.max_duration_seconds {
            requested_timeout_ms.min(seconds.max(1).saturating_mul(1000))
        } else {
            requested_timeout_ms
        };

        if enforce && rule.cost_class == CostClass::ExternalPaid && !operator_approved {
            return Err(WorkspaceError::ToolDetails {
                code: "EXTERNAL_PAID_COMMAND_APPROVAL_REQUIRED",
                message: "该命令可能调用真实付费上游；必须先由操作者在受信任 GUI/CLI 控制面启用付费命令。".into(),
                category: "permission",
                retryable: false,
                details: json!({
                    "stage": "command_cost_policy",
                    "cost_class": "external_paid",
                    "rule": rule_name,
                    "policy_path": POLICY_PATH,
                    "requested_timeout_ms": requested_timeout_ms,
                    "effective_timeout_ms": effective_timeout_ms,
                    "operator_setting": "external_paid_commands_enabled",
                    "classification": {
                        "source": source,
                        "executable": heuristic.executable,
                        "indicators": indicators,
                        "cost_intent": cost_intent,
                        "network_mode": network_mode
                    },
                    "recoverable": true,
                    "suggestion": "审阅命令、项目成本策略和预算后，在 Anchor GUI 或 CLI 中启用付费命令；模型参数不能作为批准凭证。"
                }),
            });
        }

        let max_runs = if rule.cost_class == CostClass::ExternalPaid {
            Some(
                rule.max_runs
                    .unwrap_or(policy.external_paid_max_runs_per_day)
                    .min(policy.external_paid_max_runs_per_day)
                    .max(1),
            )
        } else {
            rule.max_runs.filter(|runs| *runs > 0)
        };
        let run_number = if enforce {
            if let Some(max_runs) = max_runs {
                Some(self.reserve_run(command, rule_name.as_deref(), max_runs)?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(CommandCostDecision {
            cost_class: rule.cost_class,
            rule_name,
            requested_timeout_ms,
            effective_timeout_ms,
            confirmation_required,
            operator_approved,
            max_runs_per_day: max_runs,
            run_number,
            max_retries: rule.max_retries,
            max_external_calls: rule.max_external_calls,
            max_tokens: rule.max_tokens,
            max_cost_usd: rule.max_cost_usd,
            source,
            executable: heuristic.executable,
            indicators,
            cost_intent: cost_intent.to_string(),
            network_mode: network_mode.to_string(),
        })
    }

    fn reserve_run(
        &self,
        command: &str,
        rule_name: Option<&str>,
        max_runs: u64,
    ) -> Result<u64, WorkspaceError> {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let command_digest = digest(command.as_bytes());
        let key = format!("{date}:{}:{command_digest}", rule_name.unwrap_or("default"));
        let mut state = self.state.lock().expect("command cost state lock");
        let current = state.runs.get(&key).copied().unwrap_or(0);
        if current >= max_runs {
            return Err(WorkspaceError::ToolDetails {
                code: "EXTERNAL_PAID_COMMAND_BUDGET_EXCEEDED",
                message: format!("该命令今天已达到最大运行次数 {max_runs}。"),
                category: "policy",
                retryable: false,
                details: json!({
                    "stage": "command_cost_policy",
                    "reason": "daily_run_budget_exceeded",
                    "date": date,
                    "rule": rule_name,
                    "max_runs_per_day": max_runs,
                    "completed_or_reserved_runs": current,
                    "recoverable": false,
                    "suggestion": "检查已有运行结果；确需增加预算时由操作者修改受信任运行配置后重启服务。"
                }),
            });
        }
        let next = current + 1;
        state.runs.insert(key, next);
        persist_state(&self.state_path, &state)?;
        Ok(next)
    }
}

fn load_policy(workspace_root: &Path) -> Result<CommandPolicyFile, WorkspaceError> {
    let path = workspace_root.join(POLICY_PATH);
    if !path.exists() {
        return Ok(CommandPolicyFile::default());
    }
    let text = fs::read_to_string(&path).map_err(|error| WorkspaceError::ToolDetails {
        code: "COMMAND_COST_POLICY_INVALID",
        message: format!("无法读取 {}: {error}", path.display()),
        category: "policy",
        retryable: false,
        details: json!({"policy_path": POLICY_PATH, "reason": "read_failed"}),
    })?;
    serde_yaml::from_str(&text).map_err(|error| WorkspaceError::ToolDetails {
        code: "COMMAND_COST_POLICY_INVALID",
        message: format!("无法解析 {POLICY_PATH}: {error}"),
        category: "policy",
        retryable: false,
        details: json!({
            "policy_path": POLICY_PATH,
            "reason": "yaml_parse_failed",
            "suggestion": "修复命令策略 YAML 后重试；策略无效时 Anchor 不会忽略并继续执行。"
        }),
    })
}

fn match_rule(
    policy: &CommandPolicyFile,
    command: &str,
) -> Result<Option<(String, CommandPolicyRule)>, WorkspaceError> {
    for (name, rule) in &policy.commands {
        if rule.pattern.trim().is_empty() {
            continue;
        }
        let regex = Regex::new(&rule.pattern).map_err(|error| WorkspaceError::ToolDetails {
            code: "COMMAND_COST_POLICY_INVALID",
            message: format!("命令策略 {name} 的 match 不是有效正则表达式: {error}"),
            category: "policy",
            retryable: false,
            details: json!({
                "policy_path": POLICY_PATH,
                "rule": name,
                "pattern": rule.pattern,
                "reason": "invalid_regex"
            }),
        })?;
        if regex.is_match(command) {
            return Ok(Some((name.clone(), rule.clone())));
        }
    }
    Ok(None)
}

#[derive(Debug)]
struct CommandCostHeuristic {
    cost_class: CostClass,
    executable: Option<String>,
    indicators: Vec<String>,
}

fn validate_declarations(cost_intent: &str, network_mode: &str) -> Result<(), WorkspaceError> {
    if !matches!(cost_intent, "auto" | "local_only" | "external_paid") {
        return Err(WorkspaceError::invalid_argument(
            "cost_intent must be auto, local_only, or external_paid",
        ));
    }
    if !matches!(network_mode, "auto" | "disabled" | "enabled") {
        return Err(WorkspaceError::invalid_argument(
            "network_mode must be auto, disabled, or enabled",
        ));
    }
    Ok(())
}

fn command_cost_heuristic(
    command: &str,
    cost_intent: &str,
    network_mode: &str,
) -> Result<CommandCostHeuristic, WorkspaceError> {
    let tokens = shell_words::split(command)
        .map_err(|_| WorkspaceError::invalid_argument("Invalid command syntax"))?;
    let executable = tokens
        .iter()
        .find(|token| !looks_like_env_assignment(token))
        .map(|token| {
            Path::new(token)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(token)
                .trim_end_matches(".exe")
                .to_ascii_lowercase()
        });
    let mut indicators = Vec::new();
    if cost_intent == "external_paid" {
        indicators.push("declared_external_paid".to_string());
    }
    for token in &tokens {
        let upper = token.to_ascii_uppercase();
        if matches!(upper.as_str(), "LIVE=1" | "REAL_MODEL=1" | "E2E_LIVE=1") {
            indicators.push(format!("exact_runtime_flag:{upper}"));
        }
        if token.contains("api.openai.com")
            || token.contains("api.anthropic.com")
            || token.contains("generativelanguage.googleapis.com")
        {
            indicators.push(format!("known_paid_api_host:{token}"));
        }
    }
    let external_paid = !indicators.is_empty();
    if external_paid && (cost_intent == "local_only" || network_mode == "disabled") {
        return Err(WorkspaceError::ToolDetails {
            code: "COMMAND_COST_DECLARATION_CONFLICT",
            message: "Command contains explicit paid/network evidence that conflicts with local-only declarations.".into(),
            category: "policy",
            retryable: false,
            details: json!({
                "stage": "command_cost_policy",
                "executable": executable,
                "indicators": indicators,
                "cost_intent": cost_intent,
                "network_mode": network_mode,
                "recoverable": true,
                "suggestion": "Remove the explicit paid/network marker for a local test, or declare cost_intent=external_paid and network_mode=enabled."
            }),
        });
    }
    Ok(CommandCostHeuristic {
        cost_class: if external_paid {
            CostClass::ExternalPaid
        } else {
            CostClass::Free
        },
        executable,
        indicators,
    })
}

fn looks_like_env_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn persist_state(path: &Path, state: &CostState) -> Result<(), WorkspaceError> {
    let parent = path.parent().ok_or_else(|| WorkspaceError::Tool {
        code: "COMMAND_COST_STATE_FAILED",
        message: "命令成本状态路径无父目录".into(),
        category: "runtime",
        retryable: true,
    })?;
    fs::create_dir_all(parent).map_err(|error| WorkspaceError::ToolDetails {
        code: "COMMAND_COST_STATE_FAILED",
        message: format!("无法创建命令成本状态目录: {error}"),
        category: "runtime",
        retryable: true,
        details: json!({"path": parent.display().to_string()}),
    })?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| WorkspaceError::Tool {
        code: "COMMAND_COST_STATE_FAILED",
        message: error.to_string(),
        category: "runtime",
        retryable: true,
    })?;
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(|error| WorkspaceError::ToolDetails {
        code: "COMMAND_COST_STATE_FAILED",
        message: format!("无法写入命令成本状态: {error}"),
        category: "runtime",
        retryable: true,
        details: json!({"path": temp.display().to_string()}),
    })?;
    fs::rename(&temp, path).map_err(|error| WorkspaceError::ToolDetails {
        code: "COMMAND_COST_STATE_FAILED",
        message: format!("无法提交命令成本状态: {error}"),
        category: "runtime",
        retryable: true,
        details: json!({"path": path.display().to_string()}),
    })
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn heuristic_paid_command_requires_operator_approval() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let guard = CommandCostGuard::new(harness.path(), workspace.path());
        let error = guard
            .evaluate(
                workspace.path(),
                "pnpm story-live REAL_MODEL=1",
                30_000,
                "auto",
                "auto",
                &PolicySettings::default(),
            )
            .expect_err("paid command must be blocked");
        assert_eq!(
            error.to_error_value()["code"],
            "EXTERNAL_PAID_COMMAND_APPROVAL_REQUIRED"
        );
    }

    #[test]
    fn workspace_policy_clamps_duration_and_enforces_daily_runs() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        fs::create_dir_all(workspace.path().join(".anchor")).expect("policy dir");
        fs::write(
            workspace.path().join(POLICY_PATH),
            r#"commands:
  story-live:
    match: "story-live"
    cost_class: external_paid
    require_confirmation: true
    max_runs: 1
    max_duration_seconds: 12
"#,
        )
        .expect("policy");
        let guard = CommandCostGuard::new(harness.path(), workspace.path());
        let policy = PolicySettings {
            external_paid_commands_enabled: true,
            external_paid_max_runs_per_day: 2,
            external_paid_max_duration_seconds: 60,
            ..PolicySettings::default()
        };
        let decision = guard
            .evaluate(
                workspace.path(),
                "pnpm story-live",
                30_000,
                "auto",
                "auto",
                &policy,
            )
            .expect("first run");
        assert_eq!(decision.effective_timeout_ms(), 12_000);
        assert_eq!(decision.to_value()["run_number"], 1);
        let error = guard
            .evaluate(
                workspace.path(),
                "pnpm story-live",
                30_000,
                "auto",
                "auto",
                &policy,
            )
            .expect_err("second run blocked");
        assert_eq!(
            error.to_error_value()["code"],
            "EXTERNAL_PAID_COMMAND_BUDGET_EXCEEDED"
        );
    }

    #[test]
    fn operator_paid_run_limit_supports_values_above_u32() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let guard = CommandCostGuard::new(harness.path(), workspace.path());
        let max_runs = u64::from(u32::MAX) + 1;
        let policy = PolicySettings {
            external_paid_commands_enabled: true,
            external_paid_max_runs_per_day: max_runs,
            ..PolicySettings::default()
        };

        let decision = guard
            .evaluate(
                workspace.path(),
                "pnpm story-live REAL_MODEL=1",
                30_000,
                "auto",
                "auto",
                &policy,
            )
            .expect("large paid run limit");

        assert_eq!(decision.to_value()["max_runs_per_day"], max_runs);
        assert_eq!(decision.to_value()["run_number"], 1);
    }

    #[test]
    fn local_commands_are_not_paid_because_arguments_contain_model_words() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let guard = CommandCostGuard::new(harness.path(), workspace.path());
        for command in [
            "git add tests/story-live-model.test.ts",
            "rg DeepSeek src",
            "pnpm vitest StoryLiveModel",
            "go test ./... -run GPTModelOrdering",
        ] {
            let decision = guard
                .evaluate(
                    workspace.path(),
                    command,
                    30_000,
                    "local_only",
                    "disabled",
                    &PolicySettings::default(),
                )
                .expect("local command");
            assert_eq!(decision.to_value()["cost_class"], "free", "{command}");
        }
    }

    #[test]
    fn local_only_declaration_rejects_an_explicit_real_model_flag() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let guard = CommandCostGuard::new(harness.path(), workspace.path());
        let error = guard
            .evaluate(
                workspace.path(),
                "pnpm test REAL_MODEL=1",
                30_000,
                "local_only",
                "disabled",
                &PolicySettings::default(),
            )
            .expect_err("declaration conflict");
        assert_eq!(
            error.to_error_value()["code"],
            "COMMAND_COST_DECLARATION_CONFLICT"
        );
    }
}
