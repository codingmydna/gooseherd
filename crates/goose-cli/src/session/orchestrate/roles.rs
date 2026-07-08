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
        provider_name: config
            .get_goose_provider()
            .map_err(|e| anyhow::anyhow!("No provider configured: {}", e))?,
        model: config
            .get_goose_model()
            .map_err(|e| anyhow::anyhow!("No model configured: {}", e))?,
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

pub(super) fn role_system_prompt(base: &str, role: &RoleConfig) -> String {
    build_role_system_prompt(base, role, playbook_mode())
}

fn build_role_system_prompt(base: &str, role: &RoleConfig, mode: InjectionMode) -> String {
    if !exemplars::should_inject(&role.provider_name, &role.model, mode) {
        return base.to_string();
    }

    let preamble = if exemplars::is_fable_model(&role.provider_name, &role.model) {
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
}
