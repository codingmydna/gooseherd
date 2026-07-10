use super::super::roles::RoleConfig;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
struct SilentProvider {
    first_text: Option<&'static str>,
}

#[async_trait::async_trait]
impl goose::providers::base::Provider for SilentProvider {
    fn get_name(&self) -> &str {
        "silent-provider"
    }

    async fn stream(
        &self,
        _model_config: &goose_providers::model::ModelConfig,
        _system: &str,
        _messages: &[goose::conversation::message::Message],
        _tools: &[rmcp::model::Tool],
    ) -> std::result::Result<
        goose::providers::base::MessageStream,
        goose_providers::errors::ProviderError,
    > {
        use futures::StreamExt;

        let first_text = self.first_text;
        let pending = futures::stream::pending();
        if let Some(first_text) = first_text {
            let first = futures::stream::once(async move {
                Ok((
                    Some(goose::conversation::message::Message::assistant().with_text(first_text)),
                    None,
                ))
            });
            Ok(Box::pin(first.chain(pending)))
        } else {
            Ok(Box::pin(pending))
        }
    }
}

#[derive(Debug)]
struct PartialThenErrorProvider;

#[async_trait::async_trait]
impl goose::providers::base::Provider for PartialThenErrorProvider {
    fn get_name(&self) -> &str {
        "partial-then-error-provider"
    }

    async fn stream(
        &self,
        _model_config: &goose_providers::model::ModelConfig,
        _system: &str,
        _messages: &[goose::conversation::message::Message],
        _tools: &[rmcp::model::Tool],
    ) -> std::result::Result<
        goose::providers::base::MessageStream,
        goose_providers::errors::ProviderError,
    > {
        let items = vec![
            Ok((
                Some(goose::conversation::message::Message::assistant().with_text("partial plan")),
                None,
            )),
            Err(goose_providers::errors::ProviderError::RequestFailed(
                "Internal error: You've reached your Fable 5 limit.: { \"errorKind\": \"rate_limit\" }"
                    .to_string(),
            )),
        ];
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

#[tokio::test]
async fn stream_role_completion_returns_partial_text_after_idle_timeout() {
    let provider: Arc<dyn goose::providers::base::Provider> = Arc::new(SilentProvider {
        first_text: Some("partial plan"),
    });
    let request = goose::conversation::message::Message::user().with_text("plan this");

    let (text, usage) = super::stream_role_completion_with_idle_timeout(
        &provider,
        &goose_providers::model::ModelConfig::new("test-model"),
        "",
        std::slice::from_ref(&request),
        "test-session",
        false,
        Some(Duration::from_millis(10)),
    )
    .await
    .unwrap();

    assert_eq!(text, "partial plan");
    assert!(usage.is_none());
}

#[tokio::test]
async fn stream_role_completion_errors_when_idle_timeout_has_no_text() {
    let provider: Arc<dyn goose::providers::base::Provider> =
        Arc::new(SilentProvider { first_text: None });
    let request = goose::conversation::message::Message::user().with_text("plan this");

    let err = super::stream_role_completion_with_idle_timeout(
        &provider,
        &goose_providers::model::ModelConfig::new("test-model"),
        "",
        std::slice::from_ref(&request),
        "test-session",
        false,
        Some(Duration::from_millis(10)),
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("orchestration phase timed out after 0s without assistant text"),
        "{err}"
    );
}

#[tokio::test]
async fn stream_role_completion_error_preserves_partial_text() {
    let provider: Arc<dyn goose::providers::base::Provider> = Arc::new(PartialThenErrorProvider);
    let request = goose::conversation::message::Message::user().with_text("plan this");

    let err = match super::stream_role_completion_status(
        &provider,
        &goose_providers::model::ModelConfig::new("test-model"),
        "",
        std::slice::from_ref(&request),
        "test-session",
        false,
        None,
    )
    .await
    {
        Ok(_) => panic!("expected provider error"),
        Err(err) => err,
    };

    assert_eq!(super::partial_completion_text(&err), Some("partial plan"));
    assert!(err.chain().any(|cause| cause
        .downcast_ref::<goose_providers::errors::ProviderError>()
        .is_some()));
}

#[test]
fn archive_pending_reviews_flushes_all_review_cycles() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().display().to_string();
    let _guard = env_lock::lock_env([
        ("GOOSE_PATH_ROOT", Some(root_path)),
        ("GOOSE_REVIEW_EXEMPLARS", Some("true".to_string())),
    ]);
    let reviewer_role = RoleConfig {
        provider_name: "fable".to_string(),
        model: "fable-5".to_string(),
        effort: None,
    };
    let pending_reviews = vec![
        super::PendingReviewArchive {
            cycle: 1,
            verdict: "REVISE".to_string(),
            review_text: "VERDICT: REVISE\n\n1. Fix it.".to_string(),
            reviewer_context_limit: Some(200_000),
            reviewed_at_ms: 100,
        },
        super::PendingReviewArchive {
            cycle: 2,
            verdict: "APPROVED".to_string(),
            review_text: "VERDICT: APPROVED".to_string(),
            reviewer_context_limit: Some(200_000),
            reviewed_at_ms: 200,
        },
    ];

    super::archive_pending_reviews(
        &pending_reviews,
        "run-1",
        "Add review exemplar archive and injection",
        &reviewer_role,
    );

    let state_dir = root.path().join("state").join("review_exemplars");
    assert_eq!(
        std::fs::read_to_string(state_dir.join("run-1-review-c1.md")).expect("review c1"),
        "VERDICT: REVISE\n\n1. Fix it."
    );
    assert_eq!(
        std::fs::read_to_string(state_dir.join("run-1-review-c2.md")).expect("review c2"),
        "VERDICT: APPROVED"
    );
    let index = std::fs::read_to_string(state_dir.join("exemplars.jsonl")).expect("index");
    let records = index
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["cycle"], 1);
    assert_eq!(records[0]["verdict"], "REVISE");
    assert_eq!(records[1]["cycle"], 2);
    assert_eq!(records[1]["verdict"], "APPROVED");
}

#[test]
fn plan_round_action_respects_question_round_cap_and_toggle() {
    assert_eq!(
        super::plan_round_action(0, 2, true, false),
        super::PlanRoundAction::Finalize
    );
    assert_eq!(
        super::plan_round_action(0, 2, true, true),
        super::PlanRoundAction::Ask
    );
    assert_eq!(
        super::plan_round_action(2, 2, true, true),
        super::PlanRoundAction::Finalize
    );
    assert_eq!(
        super::plan_round_action(0, 2, false, true),
        super::PlanRoundAction::Finalize
    );
}

#[test]
fn short_headless_plan_retries_once_then_aborts() {
    assert_eq!(
        super::plan_quality_action("too short", 3000, 0),
        super::PlanQualityAction::Retry
    );
    assert_eq!(
        super::plan_quality_action("too short", 3000, 1),
        super::PlanQualityAction::Abort
    );
    assert_eq!(
        super::plan_quality_action(&"x".repeat(3000), 3000, 0),
        super::PlanQualityAction::Accept
    );
}

#[test]
fn orch_min_plan_chars_reads_env_override() {
    let _guard = env_lock::lock_env([("GOOSE_ORCH_MIN_PLAN_CHARS", Some("1200".to_string()))]);

    assert_eq!(super::orch_min_plan_chars(), 1200);
}

#[test]
fn orch_progress_cadence_defaults_to_sixty_seconds() {
    let _guard = env_lock::lock_env([("GOOSE_ORCH_PROGRESS_SECS", None::<String>)]);

    assert_eq!(super::orch_progress_cadence(), Duration::from_secs(60));
}

#[test]
fn orch_progress_cadence_reads_env_override() {
    let _guard = env_lock::lock_env([("GOOSE_ORCH_PROGRESS_SECS", Some("120".to_string()))]);

    assert_eq!(super::orch_progress_cadence(), Duration::from_secs(120));
}

#[test]
fn orch_progress_cadence_allows_zero_to_disable() {
    let _guard = env_lock::lock_env([("GOOSE_ORCH_PROGRESS_SECS", Some("0".to_string()))]);

    assert_eq!(super::orch_progress_cadence(), Duration::ZERO);
}

#[test]
fn planner_prompt_omits_question_protocol_when_disabled() {
    assert!(!super::planner_prompt(false).contains("orch-question"));
    assert!(super::planner_prompt(true).contains("orch-question"));
}

#[test]
fn review_prompt_contains_reinforced_rubric() {
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("Independent re-verification"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("acceptance criteria"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("failure-attribution"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("no-fix-needed observations"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("location"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("mechanism"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("reproduction"));
    assert!(super::REVIEW_SYSTEM_PROMPT.contains("fix direction"));
}

#[test]
fn parse_verdict_finds_line_start_verdict() {
    assert!(super::parse_verdict_approved(
        "VERDICT: APPROVED\n\nAll checks passed."
    ));
    assert!(!super::parse_verdict_approved(
        "VERDICT: REVISE\n1. Fix foo."
    ));
    assert!(!super::parse_verdict_approved("no verdict at all"));
}

#[test]
fn parse_verdict_survives_glued_preamble() {
    // Streamed assistant messages used to be concatenated without a
    // separator, so the verdict could end up mid-line after a preamble.
    assert!(super::parse_verdict_approved(
        "I'll verify the file state first.VERDICT: APPROVED\n\nDetails follow."
    ));
    assert!(!super::parse_verdict_approved(
        "Checking the diff now.VERDICT: REVISE\n1. Fix foo."
    ));
}

#[test]
fn parse_verdict_ignores_text_before_marker_on_same_line() {
    // "APPROVED" appearing before the marker must not count.
    assert!(!super::parse_verdict_approved(
        "The plan said APPROVED is expected.VERDICT: REVISE\n1. Fix foo."
    ));
}

#[test]
fn append_role_text_separates_blocks_after_tool_content() {
    let mut text = String::new();
    let mut pending = false;
    super::append_role_text(&mut text, "I'll check the fi", &mut pending);
    super::append_role_text(&mut text, "le state first.", &mut pending);
    assert_eq!(text, "I'll check the file state first.");
    pending = true; // a tool request/response arrived
    super::append_role_text(&mut text, "VERDICT: APPROVED", &mut pending);
    assert_eq!(text, "I'll check the file state first.\nVERDICT: APPROVED");
    // continuation deltas after the separator stay byte-exact
    super::append_role_text(&mut text, " — details.", &mut pending);
    assert_eq!(
        text,
        "I'll check the file state first.\nVERDICT: APPROVED — details."
    );
}

#[test]
fn append_role_text_no_separator_when_text_empty_or_newline_terminated() {
    let mut text = String::new();
    let mut pending = true;
    super::append_role_text(&mut text, "first", &mut pending);
    assert_eq!(text, "first");
    text.push('\n');
    pending = true;
    super::append_role_text(&mut text, "second", &mut pending);
    assert_eq!(text, "first\nsecond");
}
