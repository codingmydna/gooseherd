use anyhow::Result;
use goose::config::Config;

mod gates;
mod phases;
mod planner;
mod roles;
mod runner;
mod workspace;

pub(super) use roles::{build_role_provider, resolve_all_roles, RoleConfig};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrchImplementPolicy {
    Auto,
    Allowlist,
}

fn resolve_orch_implement_policy() -> Result<OrchImplementPolicy> {
    let raw = Config::global()
        .get_param::<String>(goose::acp::ORCH_IMPLEMENT_POLICY_KEY)
        .unwrap_or_else(|_| "auto".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OrchImplementPolicy::Auto),
        "allowlist" => Ok(OrchImplementPolicy::Allowlist),
        _ => anyhow::bail!(
            "{} must be one of: auto, allowlist",
            goose::acp::ORCH_IMPLEMENT_POLICY_KEY
        ),
    }
}
