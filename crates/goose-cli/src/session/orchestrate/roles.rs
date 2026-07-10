use anyhow::Result;
use goose::config::Config;
use goose::providers::base::Provider;
use std::path::Path;
use std::sync::Arc;

use super::OrchImplementPolicy;
use crate::session::exemplars::{self, InjectionMode};
use crate::session::output;

/// Distilled Fable 5 operating procedure, injected into planner/reviewer roles
/// whose serving model is not Fable.
const FABLE5_PLAYBOOK: &str = include_str!("../../../../../profiles/fable5-playbook.md");
const PLAYBOOK_MODE_KEY: &str = "GOOSE_ORCH_PLAYBOOK";
const PLAYBOOK_PREAMBLE: &str = "The following is gooseherd's operating playbook: codebase conventions and the plan -> implement -> review procedure distilled from prior orchestration runs. Treat it as project guidance to follow, not a description of your own identity.\n\n";

#[derive(Clone, PartialEq)]
pub(in crate::session) struct RoleConfig {
    pub(in crate::session) provider_name: String,
    pub(in crate::session) model: String,
    pub(in crate::session) effort: Option<String>,
}

pub(in crate::session) struct OrchRoles {
    pub(in crate::session) default: RoleConfig,
    pub(in crate::session) planner: RoleConfig,
    pub(in crate::session) reviewer: RoleConfig,
    pub(in crate::session) implementer: RoleConfig,
}

pub(in crate::session) fn resolve_all_roles() -> Result<OrchRoles> {
    let config = Config::global();
    let default = RoleConfig {
        provider_name: config.get_goose_provider().map_err(|e| {
            anyhow::anyhow!(
                "No provider configured for orchestration roles. \
                 Run `goose herd` to set up planner/implementer/reviewer roles: {}",
                e
            )
        })?,
        model: config.get_goose_model().map_err(|e| {
            anyhow::anyhow!(
                "No model configured for orchestration roles. \
                 Run `goose herd` to set up planner/implementer/reviewer roles: {}",
                e
            )
        })?,
        effort: None,
    };
    let planner = resolve_role("PLANNER", &default);
    let reviewer = resolve_role("REVIEWER", &planner);
    let implementer = resolve_role("IMPLEMENTER", &default);
    Ok(OrchRoles {
        default,
        planner,
        reviewer,
        implementer,
    })
}

/// The judge role, used by the arena judge and — as a fallback — the goal
/// evaluator. Resolves `GOOSE_JUDGE_{PROVIDER,MODEL,EFFORT}` with the reviewer as
/// its fallback, which itself falls back to the planner and then the session
/// default. Full chain: JUDGE → REVIEWER → PLANNER → session default.
pub(in crate::session) fn resolve_judge_role() -> Result<RoleConfig> {
    let roles = resolve_all_roles()?;
    Ok(resolve_role("JUDGE", &roles.reviewer))
}

fn resolve_role(prefix: &str, fallback: &RoleConfig) -> RoleConfig {
    let config = Config::global();
    RoleConfig {
        provider_name: config
            .get_param::<String>(&format!("GOOSE_{}_PROVIDER", prefix))
            .unwrap_or_else(|_| fallback.provider_name.clone()),
        model: config
            .get_param::<String>(&format!("GOOSE_{}_MODEL", prefix))
            .unwrap_or_else(|_| fallback.model.clone()),
        effort: config
            .get_param::<String>(&format!("GOOSE_{}_EFFORT", prefix))
            .ok()
            .or_else(|| fallback.effort.clone()),
    }
}

pub(in crate::session) async fn build_role_provider(
    role: &RoleConfig,
    working_dir: &Path,
) -> Result<(Arc<dyn Provider>, goose_providers::model::ModelConfig)> {
    let config = Config::global();
    let mut model_config = goose::model_config::model_config_from_user_config(
        &role.provider_name,
        role.model.as_str(),
    )?;
    if let Some(effort) = role
        .effort
        .as_deref()
        .and_then(|e| e.parse::<goose_providers::thinking::ThinkingEffort>().ok())
    {
        model_config = model_config.with_thinking_effort(effort);
    }
    let extensions = goose::config::extensions::get_enabled_extensions_with_config(config);
    let provider = goose::providers::create_with_working_dir(
        &role.provider_name,
        extensions,
        working_dir.into(),
    )
    .await?;
    Ok((provider, model_config))
}

pub(super) fn playbook_text() -> String {
    if let Ok(path) = Config::global().get_param::<String>("GOOSE_PLAYBOOK_PATH") {
        match std::fs::read_to_string(&path) {
            Ok(content) => return content,
            Err(e) => output::render_error(&format!(
                "GOOSE_PLAYBOOK_PATH ({}) unreadable, using embedded playbook: {}",
                path, e
            )),
        }
    }
    FABLE5_PLAYBOOK.to_string()
}

fn playbook_mode() -> InjectionMode {
    let raw = Config::global()
        .get_param::<String>(PLAYBOOK_MODE_KEY)
        .unwrap_or_else(|_| "auto".to_string());
    exemplars::parse_injection_mode(&raw)
}

pub(super) fn playbook_injected(role: &RoleConfig) -> bool {
    exemplars::should_inject(&role.provider_name, &role.model, playbook_mode())
}

pub(super) fn playbook_banner_fragment(role: &RoleConfig) -> String {
    if playbook_injected(role) {
        " · playbook".to_string()
    } else {
        String::new()
    }
}

/// One-line, honest explanation of why uplift is auto-skipped for a frontier
/// serving model, or `None` when uplift applies or was disabled deliberately
/// (explicit `never`/`always`). Callers print it dim so frontier users learn how
/// to override the presumption.
pub(super) fn uplift_skip_notice(role_label: &str, role: &RoleConfig) -> Option<String> {
    if playbook_mode() != InjectionMode::Auto || playbook_injected(role) {
        return None;
    }
    match exemplars::frontier_match(
        &role.provider_name,
        &role.model,
        &exemplars::frontier_patterns(),
    )? {
        exemplars::FrontierMatch::Pattern(pattern) => Some(format!(
            "uplift: skipped for {role_label} ({} matches frontier pattern '{}'; set GOOSE_UPLIFT_FRONTIER_PATTERNS to override)",
            role.model, pattern
        )),
        exemplars::FrontierMatch::ClaudeAcpDefault => Some(format!(
            "uplift: skipped for {role_label} ({}/{} presumed frontier via claude-acp default alias; set an explicit GOOSE_{}_MODEL or GOOSE_UPLIFT_FRONTIER_PATTERNS to override)",
            role.provider_name,
            role.model,
            role_label.to_ascii_uppercase()
        )),
    }
}

pub(super) fn render_uplift_skip_notice(role_label: &str, role: &RoleConfig) {
    if let Some(notice) = uplift_skip_notice(role_label, role) {
        println!("  {}", console::style(notice).dim());
    }
}

pub(super) fn is_acp_provider(provider_name: &str) -> bool {
    provider_name.ends_with("-acp")
}

pub(super) fn implement_policy_label(policy: OrchImplementPolicy, is_acp: bool) -> String {
    match (policy, is_acp) {
        (OrchImplementPolicy::Auto, _) => "auto".to_string(),
        (OrchImplementPolicy::Allowlist, true) => "allowlist".to_string(),
        (OrchImplementPolicy::Allowlist, false) => {
            "allowlist requested; native uses auto".to_string()
        }
    }
}

/// Whether the orchestration user message should carry the base role
/// instructions. Native providers receive both the instructions and the
/// operating playbook through the `system` prompt, so the user message omits
/// them. ACP providers keep the base instructions in the user message; their
/// system prompt — folded into the first prompt block by the ACP client —
/// then carries only the playbook, so the instructions are never duplicated.
pub(super) fn instructions_in_user_message(provider_name: &str) -> bool {
    is_acp_provider(provider_name)
}

/// Instruction preamble to prepend to an orchestration user message for a role,
/// empty for native providers (see [`instructions_in_user_message`]).
pub(super) fn user_instruction_preamble(instructions: &str, role: &RoleConfig) -> String {
    if instructions_in_user_message(&role.provider_name) {
        format!("{instructions}\n\n---\n\n")
    } else {
        String::new()
    }
}

/// System prompt to pass to `stream` for an orchestration planner/reviewer role.
/// Native providers receive the base instructions plus the operating playbook.
/// ACP providers receive only the playbook — their base instructions travel in
/// the user message (see [`instructions_in_user_message`]) and this system
/// prompt is folded into the first prompt block by the ACP client.
pub(super) fn role_stream_system_prompt(base: &str, role: &RoleConfig) -> String {
    if instructions_in_user_message(&role.provider_name) {
        build_role_system_prompt("", role, playbook_mode())
            .trim_start()
            .to_string()
    } else {
        build_role_system_prompt(base, role, playbook_mode())
    }
}

fn build_role_system_prompt(base: &str, role: &RoleConfig, mode: InjectionMode) -> String {
    if !exemplars::should_inject(&role.provider_name, &role.model, mode) {
        return base.to_string();
    }

    let preamble = if exemplars::is_frontier_model(
        &role.provider_name,
        &role.model,
        &exemplars::frontier_patterns(),
    ) {
        ""
    } else {
        PLAYBOOK_PREAMBLE
    };
    format!(
        "{}\n\n# Operating playbook\n\n{}{}",
        base,
        preamble,
        playbook_text()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::exemplars::InjectionMode;

    fn role(provider_name: &str, model: &str) -> RoleConfig {
        RoleConfig {
            provider_name: provider_name.to_string(),
            model: model.to_string(),
            effort: None,
        }
    }

    #[test]
    fn playbook_auto_injects_for_claude_acp_opus_with_neutral_preamble() {
        let prompt =
            build_role_system_prompt("base", &role("claude-acp", "opus"), InjectionMode::Auto);

        assert!(prompt.contains("# Operating playbook"));
        assert!(prompt.contains(PLAYBOOK_PREAMBLE));
    }

    #[test]
    fn playbook_auto_skips_fable_models() {
        assert_eq!(
            build_role_system_prompt("base", &role("claude-acp", "default"), InjectionMode::Auto),
            "base"
        );
        assert_eq!(
            build_role_system_prompt(
                "base",
                &role("claude-acp", "claude-fable-5"),
                InjectionMode::Auto
            ),
            "base"
        );
    }

    #[test]
    fn playbook_always_injects_into_fable_without_neutral_preamble() {
        let prompt = build_role_system_prompt(
            "base",
            &role("claude-acp", "claude-fable-5"),
            InjectionMode::Always,
        );

        assert!(prompt.contains("# Operating playbook"));
        assert!(!prompt.contains(PLAYBOOK_PREAMBLE));
    }

    #[test]
    fn playbook_auto_injects_for_non_acp_non_fable_with_neutral_preamble() {
        let prompt =
            build_role_system_prompt("base", &role("openai", "gpt-5.5"), InjectionMode::Auto);

        assert!(prompt.contains("# Operating playbook"));
        assert!(prompt.contains(PLAYBOOK_PREAMBLE));
    }

    #[test]
    fn uplift_skip_notice_explains_frontier_skip_in_auto_mode() {
        let _guard = env_lock::lock_env([
            ("GOOSE_ORCH_PLAYBOOK", Some("auto".to_string())),
            ("GOOSE_UPLIFT_FRONTIER_PATTERNS", Some("fable".to_string())),
        ]);

        let notice = uplift_skip_notice("planner", &role("anthropic", "claude-fable-5"))
            .expect("frontier model should produce a skip notice");
        assert!(notice.contains("planner"));
        assert!(notice.contains("frontier pattern 'fable'"));
        assert!(notice.contains("GOOSE_UPLIFT_FRONTIER_PATTERNS"));

        assert!(uplift_skip_notice("planner", &role("openai", "gpt-5.5")).is_none());

        let acp = uplift_skip_notice("implementer", &role("claude-acp", "default"))
            .expect("claude-acp default alias should produce a notice");
        assert!(acp.contains("claude-acp default alias"));
    }

    #[test]
    fn uplift_skip_notice_silent_under_explicit_mode() {
        let _guard = env_lock::lock_env([("GOOSE_ORCH_PLAYBOOK", Some("never".to_string()))]);
        assert!(uplift_skip_notice("reviewer", &role("openai", "gpt-5.5")).is_none());
    }

    #[test]
    fn instructions_kept_in_user_message_only_for_acp_providers() {
        assert!(instructions_in_user_message("claude-acp"));
        assert!(instructions_in_user_message("codex-acp"));
        assert!(!instructions_in_user_message("openai"));
        assert!(!instructions_in_user_message("anthropic"));
    }

    #[test]
    fn user_instruction_preamble_present_for_acp_absent_for_native() {
        assert_eq!(
            user_instruction_preamble("INSTRUCTIONS", &role("claude-acp", "opus")),
            "INSTRUCTIONS\n\n---\n\n"
        );
        assert_eq!(
            user_instruction_preamble("INSTRUCTIONS", &role("openai", "gpt-5.5")),
            ""
        );
    }

    #[test]
    fn native_stream_system_carries_instructions_acp_carries_playbook_only() {
        // Native, non-fable: full instructions + playbook in the system prompt.
        let native = role_stream_system_prompt_with_mode(
            "INSTRUCTIONS",
            &role("openai", "gpt-5.5"),
            InjectionMode::Auto,
        );
        assert!(native.contains("INSTRUCTIONS"));
        assert!(native.contains("# Operating playbook"));

        // ACP, non-fable: playbook only — the base instructions travel in the
        // user message, so the system prompt must not duplicate them.
        let acp = role_stream_system_prompt_with_mode(
            "INSTRUCTIONS",
            &role("claude-acp", "opus"),
            InjectionMode::Auto,
        );
        assert!(!acp.contains("INSTRUCTIONS"));
        assert!(acp.starts_with("# Operating playbook"));

        // ACP, fable: no playbook injection, so the system prompt is empty and
        // the folding step becomes a no-op.
        let acp_fable = role_stream_system_prompt_with_mode(
            "INSTRUCTIONS",
            &role("claude-acp", "claude-fable-5"),
            InjectionMode::Auto,
        );
        assert_eq!(acp_fable, "");
    }

    fn role_stream_system_prompt_with_mode(
        base: &str,
        role: &RoleConfig,
        mode: InjectionMode,
    ) -> String {
        if instructions_in_user_message(&role.provider_name) {
            build_role_system_prompt("", role, mode)
                .trim_start()
                .to_string()
        } else {
            build_role_system_prompt(base, role, mode)
        }
    }
}
