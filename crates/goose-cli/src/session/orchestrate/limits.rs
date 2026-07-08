use goose::utils::safe_truncate;
use goose_providers::errors::ProviderError;

use crate::session::{ledger, output};

use super::phases::{archive_pending_reviews, PendingReviewArchive};
use super::roles::RoleConfig;
use super::OrchOutcome;

pub(super) fn handle_phase_error(
    err: anyhow::Error,
    role: &str,
    role_cfg: &RoleConfig,
    run_id: &str,
    task: &str,
    reviewer_role: &RoleConfig,
    pending_reviews: &[PendingReviewArchive],
) -> anyhow::Result<OrchOutcome> {
    if is_limit_error(&err) {
        output::hide_thinking();
        archive_pending_reviews(pending_reviews, run_id, task, reviewer_role);
        render_limit_error(role, role_cfg, &err, run_id);
        Ok(OrchOutcome::LimitError)
    } else {
        Err(err)
    }
}

pub(super) fn is_limit_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(provider_error) = cause.downcast_ref::<ProviderError>() {
            if matches!(
                provider_error.telemetry_type(),
                "rate_limit" | "credits_exhausted" | "auth"
            ) {
                return true;
            }

            return match provider_error {
                ProviderError::RequestFailed(message)
                | ProviderError::ServerError(message)
                | ProviderError::ExecutionError(message) => message_signals_limit(message),
                _ => false,
            };
        }
    }
    message_signals_limit(&format!("{err:#}"))
}

fn message_signals_limit(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    const SIGNALS: &[&str] = &[
        "rate limit",
        "rate_limit",
        "ratelimit",
        "quota",
        "usage limit",
        "usage-credits",
        "too many requests",
        "reached your",
        "credit",
        "insufficient",
        "unauthorized",
        "authentication",
        "auth error",
        "invalid api key",
    ];
    SIGNALS.iter().any(|signal| message.contains(signal))
}

fn paraphrase(err: &anyhow::Error) -> String {
    let full = err.to_string();
    let summary = full
        .split(['{', '\n'])
        .next()
        .unwrap_or(&full)
        .trim()
        .trim_end_matches([':', ' ']);
    safe_truncate(summary, 200)
}

fn render_limit_error(role: &str, role_cfg: &RoleConfig, err: &anyhow::Error, run_id: &str) {
    let env_role = role.to_uppercase();
    output::render_error(&format!(
        "{role} ({provider}/{model}) hit a provider usage/auth limit and orch could not continue:\n  {summary}\n\nYour work was preserved. Run artifacts are in .goose-orch/{run_id}/ and the run ledger is {ledger}.\n\nTo recover, switch the {role}'s model, then re-run or resume:\n  - in this session: /roles {role}={provider}/<new-model>\n  - or set GOOSE_{env_role}_PROVIDER / GOOSE_{env_role}_MODEL, then re-run `goose orch` or `/orch`.\n\nOrch did not retry automatically; choose the replacement model explicitly.",
        role = role,
        provider = role_cfg.provider_name,
        model = role_cfg.model,
        summary = paraphrase(err),
        run_id = run_id,
        ledger = ledger::path_display(),
        env_role = env_role,
    ));
}

#[cfg(test)]
mod tests {
    use goose_providers::errors::ProviderError;

    #[test]
    fn structured_provider_limit_errors_are_classified() {
        let rate_limit = anyhow::Error::new(ProviderError::RateLimitExceeded {
            details: "provider throttle".to_string(),
            retry_delay: None,
        });
        assert!(super::is_limit_error(&rate_limit));

        let credits = anyhow::Error::new(ProviderError::CreditsExhausted {
            details: "credits exhausted".to_string(),
            top_up_url: None,
        });
        assert!(super::is_limit_error(&credits));

        let auth = anyhow::Error::new(ProviderError::Authentication("invalid api key".to_string()));
        assert!(super::is_limit_error(&auth));
    }

    #[test]
    fn acp_and_untyped_limit_messages_are_classified() {
        let acp_limit = anyhow::Error::new(ProviderError::RequestFailed(
            "Internal error: You've reached your Fable 5 limit. Run /usage-credits to continue or switch models with /model.: { \"errorKind\": \"rate_limit\" }"
                .to_string(),
        ));
        assert!(super::is_limit_error(&acp_limit));

        let plain_limit = anyhow::anyhow!("Request failed: provider rate limit exceeded");
        assert!(super::is_limit_error(&plain_limit));
    }

    #[test]
    fn non_limit_failures_are_not_classified() {
        let empty_plan = anyhow::anyhow!("planner produced an empty plan");
        assert!(!super::is_limit_error(&empty_plan));

        let timeout =
            anyhow::anyhow!("orchestration phase timed out after 600s without assistant text");
        assert!(!super::is_limit_error(&timeout));

        let build_failure = anyhow::anyhow!(
            "error[E0429]: `self` imports are only allowed within a {{ }} list\nerror[E0401]: can't use `Self` from outer item"
        );
        assert!(!super::is_limit_error(&build_failure));

        let rejection = anyhow::anyhow!("VERDICT: REVISE\n1. Fix the null check in foo.rs");
        assert!(!super::is_limit_error(&rejection));

        let context_length = anyhow::Error::new(ProviderError::ContextLengthExceeded(
            "context window exceeded".to_string(),
        ));
        assert!(!super::is_limit_error(&context_length));
    }

    #[test]
    fn paraphrase_strips_raw_json_payload() {
        let err = anyhow::Error::new(ProviderError::RequestFailed(
            "Internal error: You've reached your Fable 5 limit.: { \"errorKind\": \"rate_limit\" }"
                .to_string(),
        ));

        let summary = super::paraphrase(&err);

        assert!(summary.contains("reached your Fable 5 limit"));
        assert!(!summary.contains("errorKind"));
        assert!(!summary.contains('{'));
    }
}
