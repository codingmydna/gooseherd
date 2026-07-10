use anyhow::Result;
use goose::config::Config;

mod gates;
mod limits;
mod phases;
mod planner;
mod repo_pack;
mod roles;
mod runner;
mod workspace;

pub(crate) use gates::{gate_banner_line, resolve_gates, seed_allowed_commands, GateSource};
pub(super) use phases::orch_phase_idle_timeout;
pub(super) use roles::{build_role_provider, resolve_all_roles, resolve_judge_role, RoleConfig};
pub(super) use workspace::git_evidence;

const MAX_CYCLES_KEY: &str = "GOOSE_ORCH_MAX_CYCLES";
const GATES_KEY: &str = "GOOSE_ORCH_GATES";
const MAX_GATE_RETRIES_KEY: &str = "GOOSE_ORCH_MAX_GATE_RETRIES";
const DEFAULT_MAX_CYCLES: u32 = 3;
const DEFAULT_MAX_GATE_RETRIES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchOutcome {
    Approved,
    MaxCycles,
    GateFailed,
    Aborted,
    LimitError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrchImplementPolicy {
    Auto,
    Allowlist,
}

impl OrchImplementPolicy {
    /// The `GOOSE_ORCH_IMPLEMENT_POLICY` string the provider's
    /// `ImplementPolicy::from_config` expects, so the resolved policy actually
    /// reaches the ACP implementer instead of defaulting to `auto`.
    fn as_config_str(self) -> &'static str {
        match self {
            OrchImplementPolicy::Auto => "auto",
            OrchImplementPolicy::Allowlist => "allowlist",
        }
    }
}

/// Resolve the orchestration implement policy. An explicit
/// `GOOSE_ORCH_IMPLEMENT_POLICY` is always honored. When unset, the default is
/// safe-by-default: headless runs (no human watching approvals) get the
/// workspace `allowlist`; interactive runs keep `auto`, since the user is
/// present to approve.
fn resolve_orch_implement_policy(interactive: bool) -> Result<OrchImplementPolicy> {
    let raw = match Config::global().get_param::<String>(goose::acp::ORCH_IMPLEMENT_POLICY_KEY) {
        Ok(raw) => raw,
        Err(_) => {
            return Ok(if interactive {
                OrchImplementPolicy::Auto
            } else {
                OrchImplementPolicy::Allowlist
            });
        }
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OrchImplementPolicy::Auto),
        "allowlist" => Ok(OrchImplementPolicy::Allowlist),
        _ => anyhow::bail!(
            "{} must be one of: auto, allowlist",
            goose::acp::ORCH_IMPLEMENT_POLICY_KEY
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implement_policy_defaults_safe_by_headlessness_and_honors_explicit() {
        let _guard = env_lock::lock_env([("GOOSE_ORCH_IMPLEMENT_POLICY", None::<&str>)]);

        // Unset: headless is workspace-scoped, interactive stays permissive.
        assert_eq!(
            resolve_orch_implement_policy(false).unwrap(),
            OrchImplementPolicy::Allowlist
        );
        assert_eq!(
            resolve_orch_implement_policy(true).unwrap(),
            OrchImplementPolicy::Auto
        );

        // Explicit value always wins, regardless of interactivity.
        std::env::set_var("GOOSE_ORCH_IMPLEMENT_POLICY", "auto");
        assert_eq!(
            resolve_orch_implement_policy(false).unwrap(),
            OrchImplementPolicy::Auto
        );
        std::env::set_var("GOOSE_ORCH_IMPLEMENT_POLICY", "allowlist");
        assert_eq!(
            resolve_orch_implement_policy(true).unwrap(),
            OrchImplementPolicy::Allowlist
        );
        std::env::set_var("GOOSE_ORCH_IMPLEMENT_POLICY", "nonsense");
        assert!(resolve_orch_implement_policy(false).is_err());
    }
}
