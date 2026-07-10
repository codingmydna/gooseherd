use anyhow::Result;
use goose::config::Config;
use goose::conversation::message::Message;
use goose::providers::base::{Provider, ProviderUsage};
use goose::utils::safe_truncate;
use goose_providers::errors::ProviderError;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::session::{exemplars, ledger, orch_ask, output, review_exemplars};

use super::roles::{playbook_injected, RoleConfig};

const PHASE_IDLE_TIMEOUT_KEY: &str = "GOOSE_ORCH_PHASE_IDLE_TIMEOUT_SECS";
const MIN_PLAN_CHARS_KEY: &str = "GOOSE_ORCH_MIN_PLAN_CHARS";
const MAX_QUESTIONS_KEY: &str = "GOOSE_ORCH_MAX_QUESTIONS";
const ASK_KEY: &str = "GOOSE_ORCH_ASK";
const PROGRESS_SECS_KEY: &str = "GOOSE_ORCH_PROGRESS_SECS";
const DEFAULT_PHASE_IDLE_TIMEOUT_SECS: u64 = 600;
const DEFAULT_MIN_PLAN_CHARS: usize = 3_000;
const DEFAULT_MAX_QUESTION_ROUNDS: u32 = 2;
const DEFAULT_PROGRESS_SECS: u64 = 60;
pub(super) const EVIDENCE_CHAR_LIMIT: usize = 30_000;

const PLAN_SYSTEM_PROMPT: &str = r#"You are the planning lead in a two-model workflow. A separate implementer model will execute your plan with file-editing and shell tools. Your session is read-only: you can explore the working directory but cannot modify anything.

Produce a concrete, step-by-step implementation plan for the given task. Explore freely first: read files, search, and delegate read-only subagent explorations (in parallel when useful). File modifications will be denied by policy; shell commands are denied unless the session allows them — do not retry denied calls. Even if some exploration is blocked, always deliver your best plan from what you could read. Do not implement the changes yourself.

Structure the plan with exactly these markdown sections, in this order:
## Files
The files to create or modify, each with the change it needs.
## Steps
The ordered implementation steps.
## Acceptance criteria
Each criterion a concrete, checkable statement of done.
## Verification
The commands the implementer should run and the expected outcome of each.

Output only the plan."#;

const PLAN_QUESTION_PROTOCOL_PROMPT: &str = r#"Planner question protocol:

Before writing the plan, ask the user questions only when their answer would materially change the plan: missing requirements from the original task, an important double-check, or multiple sound implementation approaches. To ask, output only a fenced block and end your turn:

```orch-question
{"questions":[{"header":"short tab label, 12 chars max","question":"...","recommended":0,"options":[{"label":"...","description":"Include pros/cons and explain why this is recommended when it is the recommended option.","preview":"optional multi-line ASCII mockup or code snippet for design/layout questions"}]}]}
```

Ask 1-3 questions at a time. Each question must have 2-4 options. Put your own preferred option index in recommended and justify that recommendation in the option description. Do not output a plan in the same turn as questions. Once answers arrive, or if the user declines to answer, produce the plan as usual. Prefer deciding yourself; ask only when necessary."#;

pub(super) const REVIEW_SYSTEM_PROMPT: &str = r#"You are the reviewing lead in a two-model workflow. An implementer model has just attempted the task. You receive the original task, the plan, the git evidence of what changed, and the implementer's report. Your session is read-only: you can inspect files in the working directory but cannot modify anything.

Judge whether the implementation correctly and completely fulfills the task and plan. Inspect files in the working directory if the evidence is insufficient. Some tool calls (especially shell commands) may be denied by policy; do not retry them — judge from file reads and the provided evidence instead. You must always deliver a verdict.

Review rubric:
- Independent re-verification: when possible, directly open files and run the relevant gates yourself; if denied, say what evidence you used instead.
- Judge plan deviations against the task and plan acceptance criteria, not against incidental plan wording.
- For any failure, make a failure-attribution judgment: implementation defect, plan ambiguity, external/tool failure, or insufficient evidence; block only when the current implementation fails the task.
- Keep no-fix-needed observations separate from blocking defects.
- If REVISE, use a numbered list where each defect includes location, mechanism, reproduction/evidence, and fix direction.

Your reply MUST start with exactly one of these lines:
VERDICT: APPROVED
VERDICT: REVISE

If REVISE, follow with a numbered list of concrete, actionable defects (file, problem, required fix). Only demand changes for real problems; do not invent nitpicks."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanRoundAction {
    Ask,
    Finalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanQualityAction {
    Accept,
    Retry,
    Abort,
}

pub(super) fn plan_round_action(
    round: u32,
    max_question_rounds: u32,
    ask_enabled: bool,
    has_question: bool,
) -> PlanRoundAction {
    if ask_enabled && has_question && round < max_question_rounds {
        PlanRoundAction::Ask
    } else {
        PlanRoundAction::Finalize
    }
}

pub(super) fn plan_quality_action(
    plan_text: &str,
    min_chars: usize,
    short_retries: u32,
) -> PlanQualityAction {
    if min_chars == 0 || plan_text.trim().chars().count() >= min_chars {
        PlanQualityAction::Accept
    } else if short_retries == 0 {
        PlanQualityAction::Retry
    } else {
        PlanQualityAction::Abort
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanSection {
    Files,
    Steps,
    AcceptanceCriteria,
    Verification,
}

impl PlanSection {
    const ALL: [PlanSection; 4] = [
        PlanSection::Files,
        PlanSection::Steps,
        PlanSection::AcceptanceCriteria,
        PlanSection::Verification,
    ];

    fn keyword(self) -> &'static str {
        match self {
            PlanSection::Files => "files",
            PlanSection::Steps => "steps",
            PlanSection::AcceptanceCriteria => "acceptance criteria",
            PlanSection::Verification => "verification",
        }
    }

    pub(super) fn header(self) -> &'static str {
        match self {
            PlanSection::Files => "## Files",
            PlanSection::Steps => "## Steps",
            PlanSection::AcceptanceCriteria => "## Acceptance criteria",
            PlanSection::Verification => "## Verification",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanStructureAction {
    Accept,
    Reprompt,
    ProceedWithWarning,
}

/// Required plan sections absent from `plan`. Tolerant: headers match
/// case-insensitively, both `## ` and `### ` are accepted, extra sections are
/// allowed, and a header matches a section when its text begins with the
/// section name (so `## Verification commands` satisfies Verification).
pub(super) fn validate_plan_structure(plan: &str) -> Vec<PlanSection> {
    let headers: Vec<String> = plan.lines().filter_map(normalized_plan_header).collect();
    PlanSection::ALL
        .into_iter()
        .filter(|section| {
            !headers
                .iter()
                .any(|header| header.starts_with(section.keyword()))
        })
        .collect()
}

fn normalized_plan_header(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("##") {
        return None;
    }
    let text = trimmed.trim_start_matches('#').trim();
    (!text.is_empty()).then(|| text.to_ascii_lowercase())
}

pub(super) fn plan_structure_action(
    missing: &[PlanSection],
    structure_retries: u32,
) -> PlanStructureAction {
    if missing.is_empty() {
        PlanStructureAction::Accept
    } else if structure_retries == 0 {
        PlanStructureAction::Reprompt
    } else {
        PlanStructureAction::ProceedWithWarning
    }
}

pub(super) fn missing_sections_label(missing: &[PlanSection]) -> String {
    missing
        .iter()
        .map(|section| section.header())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn plan_structure_reprompt(missing: &[PlanSection]) -> String {
    format!(
        "Your plan is missing required section(s): {}. Re-emit the COMPLETE plan (not a patch) with every required section present: ## Files, ## Steps, ## Acceptance criteria, ## Verification. Do not ask questions.",
        missing_sections_label(missing)
    )
}

/// Extract the individual acceptance-criterion lines from a structured plan's
/// `## Acceptance criteria` section (the B1 plan schema). Each returned string is
/// a trimmed criterion with any leading list marker (`-`, `*`, `+`, `1.`, `2)`)
/// stripped, in document order. Empty when the plan has no such section.
pub(super) fn extract_acceptance_criteria(plan: &str) -> Vec<String> {
    let mut criteria = Vec::new();
    let mut in_section = false;
    for line in plan.lines() {
        if let Some(header) = normalized_plan_header(line) {
            in_section = header.starts_with(PlanSection::AcceptanceCriteria.keyword());
            continue;
        }
        if !in_section {
            continue;
        }
        let item = strip_list_marker(line.trim());
        if !item.is_empty() {
            criteria.push(item.to_string());
        }
    }
    criteria
}

fn strip_list_marker(line: &str) -> &str {
    let line = line.trim();
    for marker in ['-', '*', '+'] {
        if let Some(rest) = line.strip_prefix(marker) {
            return rest.trim_start();
        }
    }
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        if let Some(rest) = line
            .get(digits.len()..)
            .and_then(|rest| rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')))
        {
            return rest.trim_start();
        }
    }
    line
}

/// Whether an implementer report carries a self-verification section. Tolerant:
/// matches a `## Self-verification` header case-insensitively, accepts `##`/`###`,
/// treats hyphen/underscore as a space (`Self verification`), and allows trailing
/// text on the header line.
pub(super) fn has_self_verification(report: &str) -> bool {
    report
        .lines()
        .filter_map(normalized_plan_header)
        .any(|header| {
            header
                .replace(['-', '_'], " ")
                .starts_with("self verification")
        })
}

fn acceptance_criteria_block(plan: &str) -> String {
    let criteria = extract_acceptance_criteria(plan);
    if criteria.is_empty() {
        return "The plan lists no explicit acceptance criteria; enumerate the task's own success conditions and map each to evidence.".to_string();
    }
    let mut block = String::from("Acceptance criteria:\n");
    for (index, criterion) in criteria.iter().enumerate() {
        block.push_str(&format!("{}. {}\n", index + 1, criterion));
    }
    block.trim_end().to_string()
}

/// The `## Self-verification` demand appended to the implementer instruction: end
/// the report with that section, mapping each acceptance criterion to concrete
/// evidence (command + observed output, or `file:line`).
pub(super) fn self_verification_demand(plan: &str) -> String {
    format!(
        "\n\nWhen you finish, END your report with a `## Self-verification` section. For EACH acceptance criterion below add one bullet mapping the criterion to concrete evidence: the exact verification command you ran and its observed output, or a `file:line` reference. Do not claim a criterion passes without evidence.\n\n{}",
        acceptance_criteria_block(plan)
    )
}

/// The one bounded reprompt sent to an implementer whose report lacked the
/// `## Self-verification` section: emit only that section for the criteria.
pub(super) fn self_verification_reprompt(plan: &str) -> String {
    format!(
        "Your report is missing the required `## Self-verification` section. Emit ONLY that section now — a `## Self-verification` header followed by one bullet per acceptance criterion, each mapping the criterion to concrete evidence (the exact command you ran and its observed output, or a `file:line`). Do not repeat the rest of your report.\n\n{}",
        acceptance_criteria_block(plan)
    )
}

/// The self-verification checklist appended to the review request. Presents the
/// implementer's mapping as claims the reviewer must confirm against the evidence,
/// and tells the reviewer to distrust any criterion it cannot independently verify.
pub(super) fn self_verification_review_block(plan: &str, report: &str) -> String {
    let criteria = extract_acceptance_criteria(plan);
    let mut block = String::from("## Self-verification checklist\n");
    if has_self_verification(report) {
        block.push_str(
            "The implementer's report ends with a `## Self-verification` section (above). Verify each claim against the git evidence and gate output — open files and re-run checks where you can. Treat any criterion whose evidence you cannot independently confirm as NOT met, and missing or vague evidence as a blocking defect.",
        );
    } else {
        block.push_str(
            "WARNING: the implementer did NOT provide a `## Self-verification` section despite being asked. Do not take the report's success claims at face value — independently verify every acceptance criterion below against the evidence, and block if you cannot confirm one.",
        );
    }
    if !criteria.is_empty() {
        block.push_str("\n\nAcceptance criteria to confirm:\n");
        for (index, criterion) in criteria.iter().enumerate() {
            block.push_str(&format!("{}. {}\n", index + 1, criterion));
        }
    }
    block.trim_end().to_string()
}

/// Ledger row noting whether the implementer report carried the self-verification
/// section, and whether a reprompt was needed. Only appended when the initial
/// report lacked the section, so /stats can measure compliance.
pub(super) fn record_self_verification(
    meta: &PhaseMeta<'_>,
    cycle: u32,
    role_cfg: &RoleConfig,
    recovered: bool,
) {
    ledger::append(&ledger::PhaseRecord {
        ts_ms: ledger::now_ms(),
        session_id: meta.session_id.to_string(),
        run_id: meta.run_id.to_string(),
        phase: "self-verify".to_string(),
        cycle,
        role: "implementer".to_string(),
        provider: role_cfg.provider_name.clone(),
        config_model: role_cfg.model.clone(),
        reported_model: None,
        context_limit: None,
        input_tokens: None,
        output_tokens: None,
        duration_ms: 0,
        verdict: Some(if recovered { "RECOVERED" } else { "MISSING" }.to_string()),
        permission_policy: None,
        permission_denials: None,
        task_preview: safe_truncate(meta.task, 120),
        plan_exemplars_injected: None,
        plan_exemplar_run_ids: None,
        review_exemplars_injected: None,
        review_exemplar_run_ids: None,
        playbook_injected: None,
        arena_rank: None,
        arena_winner: None,
    });
}

/// Appends an assistant text chunk to the collected role text. Streamed text
/// deltas of one block must concatenate byte-exactly, but a text block that
/// follows non-text content (a tool call/response) is a new message — without
/// a separator it would glue onto the previous sentence and break line-based
/// parsing such as the review verdict.
pub(super) fn append_role_text(text: &mut String, chunk: &str, separator_pending: &mut bool) {
    if *separator_pending && !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    *separator_pending = false;
    text.push_str(chunk);
}

pub(super) fn phase_banner(text: &str, role: output::ActiveRole) {
    let color = match role {
        output::ActiveRole::Planner => console::Color::Cyan,
        output::ActiveRole::Implementer => console::Color::Yellow,
        output::ActiveRole::Reviewer => console::Color::Magenta,
    };
    println!(
        "{}",
        console::style(format!("― {} ―", text)).fg(color).bold()
    );
}

pub(super) fn gate_banner(text: &str) {
    println!(
        "{}",
        console::style(format!("― {} ―", text))
            .fg(console::Color::Blue)
            .bold()
    );
}

pub(super) fn planner_prompt(ask_enabled: bool) -> String {
    if ask_enabled {
        format!("{PLAN_SYSTEM_PROMPT}\n\n{PLAN_QUESTION_PROTOCOL_PROMPT}")
    } else {
        PLAN_SYSTEM_PROMPT.to_string()
    }
}

pub(super) fn orch_phase_idle_timeout() -> Duration {
    let secs = Config::global()
        .get_param::<u64>(PHASE_IDLE_TIMEOUT_KEY)
        .ok()
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_PHASE_IDLE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

pub(super) fn orch_progress_cadence() -> Duration {
    let secs = Config::global()
        .get_param::<u64>(PROGRESS_SECS_KEY)
        .ok()
        .unwrap_or(DEFAULT_PROGRESS_SECS);
    Duration::from_secs(secs)
}

pub(super) fn orch_min_plan_chars() -> usize {
    Config::global()
        .get_param::<usize>(MIN_PLAN_CHARS_KEY)
        .ok()
        .unwrap_or(DEFAULT_MIN_PLAN_CHARS)
}

pub(super) fn orch_max_question_rounds() -> u32 {
    Config::global()
        .get_param::<u32>(MAX_QUESTIONS_KEY)
        .ok()
        .unwrap_or(DEFAULT_MAX_QUESTION_ROUNDS)
}

pub(super) fn orch_ask_enabled() -> bool {
    Config::global()
        .get_param::<String>(ASK_KEY)
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "false" | "0"
            )
        })
        .unwrap_or(true)
}

pub(super) fn persist_artifact(artifact_dir: &Path, run_id: &str, name: &str, content: &str) {
    let dir = artifact_dir.join(".goose-orch").join(run_id);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(name), content);
    }
}

pub(super) fn warn_truncated(what: &str, full_len: usize, run_id: &str) {
    println!(
        "  {}",
        console::style(format!(
            "⚠ {} truncated ({} chars → {}k limit) — full copy in .goose-orch/{}/",
            what,
            full_len,
            EVIDENCE_CHAR_LIMIT / 1000,
            run_id
        ))
        .yellow()
    );
}

#[derive(Debug)]
pub(super) struct RoleCompletion {
    pub(super) text: String,
    pub(super) usage: Option<ProviderUsage>,
    pub(super) idle_timed_out: bool,
}

#[derive(Debug)]
struct PartialRoleCompletionError {
    partial_text: String,
    source: ProviderError,
}

impl std::fmt::Display for PartialRoleCompletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for PartialRoleCompletionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(super) fn partial_completion_text(err: &anyhow::Error) -> Option<&str> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<PartialRoleCompletionError>())
        .map(|err| err.partial_text.as_str())
}

pub(super) async fn stream_role_completion(
    provider: &Arc<dyn Provider>,
    model_config: &goose_providers::model::ModelConfig,
    system: &str,
    messages: &[Message],
    session_id: &str,
    debug: bool,
) -> Result<(String, Option<ProviderUsage>)> {
    let completion = stream_role_completion_status(
        provider,
        model_config,
        system,
        messages,
        session_id,
        debug,
        None,
    )
    .await?;
    Ok((completion.text, completion.usage))
}

pub(super) async fn stream_role_completion_status(
    provider: &Arc<dyn Provider>,
    model_config: &goose_providers::model::ModelConfig,
    system: &str,
    messages: &[Message],
    session_id: &str,
    debug: bool,
    idle_timeout: Option<Duration>,
) -> Result<RoleCompletion> {
    use futures::StreamExt;

    let mut stream = goose::session_context::with_session_id(
        Some(session_id.to_string()),
        provider.stream(model_config, system, messages, &[]),
    )
    .await?;

    let mut buffer = crate::session::streaming_buffer::MarkdownBuffer::new();
    let mut thinking_header_shown = false;
    let mut text = String::new();
    let mut separator_pending = false;
    let mut usage: Option<ProviderUsage> = None;
    let _thinking_turn = output::begin_thinking_turn();
    let mut status_tick = tokio::time::interval(output::thinking_status_refresh_interval());
    status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let idle_sleep = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(1)));
    tokio::pin!(idle_sleep);
    let mut timeout_error: Option<String> = None;
    let mut stream_error: Option<ProviderError> = None;
    let mut idle_timed_out = false;

    loop {
        tokio::select! {
            next = stream.next() => {
                let Some(next) = next else {
                    break;
                };
                let (message, message_usage) = match next {
                    Ok(next) => next,
                    Err(err) => {
                        stream_error = Some(err);
                        break;
                    }
                };
                if let Some(message) = message {
                    for content in &message.content {
                        if let goose::conversation::message::MessageContent::Text(t) = content {
                            append_role_text(&mut text, &t.text, &mut separator_pending);
                        } else {
                            separator_pending = true;
                        }
                    }
                    output::hide_thinking();
                    output::render_message_streaming(
                        &message,
                        &mut buffer,
                        &mut thinking_header_shown,
                        debug,
                    );
                }
                if message_usage.is_some() {
                    usage = message_usage;
                }
                if let Some(timeout) = idle_timeout {
                    idle_sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
                }
            }
            _ = status_tick.tick() => {
                output::phase_progress_tick();
                output::refresh_thinking_status();
            }
            _ = &mut idle_sleep, if idle_timeout.is_some() => {
                let secs = idle_timeout.unwrap().as_secs();
                if text.trim().is_empty() {
                    timeout_error = Some(format!(
                        "orchestration phase timed out after {secs}s without assistant text"
                    ));
                    break;
                }
                println!(
                    "  {}",
                    console::style(format!(
                        "orchestration phase idle for {secs}s; using collected assistant text"
                    ))
                    .yellow()
                );
                idle_timed_out = true;
                break;
            }
        }
    }
    output::flush_markdown_buffer_current_theme(&mut buffer);
    output::reset_response_bullet();
    if let Some(error) = timeout_error {
        anyhow::bail!(error);
    }
    if let Some(source) = stream_error {
        if text.trim().is_empty() {
            return Err(source.into());
        }
        return Err(PartialRoleCompletionError {
            partial_text: text,
            source,
        }
        .into());
    }
    Ok(RoleCompletion {
        text,
        usage,
        idle_timed_out,
    })
}

pub(super) struct PhaseMeta<'a> {
    pub(super) session_id: &'a str,
    pub(super) run_id: &'a str,
    pub(super) task: &'a str,
}

pub(super) struct PhasePolicySummary {
    pub(super) name: String,
    pub(super) denials: u64,
}

pub(super) struct PendingReviewArchive {
    pub(super) cycle: u32,
    pub(super) verdict: String,
    pub(super) review_text: String,
    pub(super) reviewer_context_limit: Option<usize>,
    pub(super) reviewed_at_ms: u128,
}

pub(super) fn archive_pending_reviews(
    pending_reviews: &[PendingReviewArchive],
    run_id: &str,
    task: &str,
    reviewer_role: &RoleConfig,
    repo_root: Option<&str>,
) {
    for review in pending_reviews {
        review_exemplars::archive_review(&review_exemplars::ArchiveReviewRequest {
            run_id,
            cycle: review.cycle,
            verdict: &review.verdict,
            task,
            review_text: &review.review_text,
            reviewer_provider: &reviewer_role.provider_name,
            reviewer_model: &reviewer_role.model,
            reviewer_context_limit: review.reviewer_context_limit,
            repo_root,
            reviewed_at_ms: review.reviewed_at_ms,
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_phase(
    meta: &PhaseMeta<'_>,
    phase: &str,
    cycle: u32,
    role: &str,
    role_cfg: &RoleConfig,
    usage: Option<&ProviderUsage>,
    context_limit: Option<usize>,
    elapsed_ms: u64,
    verdict: Option<&str>,
    policy: Option<&PhasePolicySummary>,
    plan_exemplar_injection: Option<&exemplars::ExemplarInjection>,
    review_exemplar_injection: Option<&exemplars::ExemplarInjection>,
) {
    let reported_model = usage.map(|u| u.model.clone());
    let (input_tokens, output_tokens) = usage
        .map(|u| {
            (
                u.usage.input_tokens.map(|n| n as i64),
                u.usage.output_tokens.map(|n| n as i64),
            )
        })
        .unwrap_or((None, None));

    let fmt_tok = |t: Option<i64>| t.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
    let policy_suffix = policy
        .map(|policy| format!(" · policy {} · denied {}", policy.name, policy.denials))
        .unwrap_or_default();
    println!(
        "  {} {}",
        console::style("⎿").dim(),
        console::style(format!(
            "{} done · model {} · in {} / out {} · {:.1}s{}{}",
            phase,
            reported_model.as_deref().unwrap_or("(unreported)"),
            fmt_tok(input_tokens),
            fmt_tok(output_tokens),
            elapsed_ms as f64 / 1000.0,
            verdict.map(|v| format!(" · {}", v)).unwrap_or_default(),
            policy_suffix
        ))
        .dim()
    );

    if let Ok(expected) =
        Config::global().get_param::<String>(&format!("GOOSE_{}_EXPECT_MODEL", role.to_uppercase()))
    {
        let generic = reported_model
            .as_deref()
            .map(exemplars::is_generic_model)
            .unwrap_or(true);
        let matched = reported_model
            .as_deref()
            .map(|m| m.to_lowercase().contains(&expected.to_lowercase()))
            .unwrap_or(false);
        if generic {
            println!(
                "  {}",
                console::style(format!(
                    "· {} model identity unverifiable (adapter reports '{}'); check ctx-limit fingerprint in /stats",
                    role,
                    reported_model.as_deref().unwrap_or("-")
                ))
                .dim()
            );
        } else if !matched {
            println!(
                "  {}",
                console::style(format!(
                    "⚠ {} reported model '{}' does not match expected '{}' — possible downgrade",
                    role,
                    reported_model.as_deref().unwrap_or("(unreported)"),
                    expected
                ))
                .yellow()
                .bold()
            );
        }
    }

    ledger::append(&ledger::PhaseRecord {
        ts_ms: ledger::now_ms(),
        session_id: meta.session_id.to_string(),
        run_id: meta.run_id.to_string(),
        phase: phase.to_string(),
        cycle,
        role: role.to_string(),
        provider: role_cfg.provider_name.clone(),
        config_model: role_cfg.model.clone(),
        reported_model,
        context_limit,
        input_tokens,
        output_tokens,
        duration_ms: elapsed_ms,
        verdict: verdict.map(|v| v.to_string()),
        permission_policy: policy.map(|policy| policy.name.clone()),
        permission_denials: policy.map(|policy| policy.denials),
        task_preview: safe_truncate(meta.task, 120),
        plan_exemplars_injected: plan_exemplar_injection.map(|injection| injection.injected),
        plan_exemplar_run_ids: plan_exemplar_injection
            .map(|injection| injection.selected_run_ids.clone()),
        review_exemplars_injected: review_exemplar_injection.map(|injection| injection.injected),
        review_exemplar_run_ids: review_exemplar_injection
            .map(|injection| injection.selected_run_ids.clone()),
        playbook_injected: Some(playbook_injected(role_cfg)),
        arena_rank: None,
        arena_winner: None,
    });
}

pub(super) fn render_auto_answer_banner(
    question_set: &orch_ask::OrchQuestionSet,
    answers: &[orch_ask::OrchAnswer],
    reason: &str,
) {
    println!(
        "  {}",
        console::style(format!("planner questions auto-answered ({reason})"))
            .yellow()
            .bold()
    );
    for answer in answers {
        let Some(question) = question_set.questions.get(answer.question_index) else {
            continue;
        };
        println!(
            "  {} {} → {}",
            console::style("·").dim(),
            question.header,
            answer_label(question_set, answer)
        );
    }
}

pub(super) fn record_question_round(
    meta: &PhaseMeta<'_>,
    round: u32,
    role_cfg: &RoleConfig,
    question_set: &orch_ask::OrchQuestionSet,
    answers: &[orch_ask::OrchAnswer],
    reason: &str,
) {
    let selected = answers
        .iter()
        .filter_map(|answer| {
            question_set
                .questions
                .get(answer.question_index)
                .map(|question| {
                    format!("{}={}", question.header, answer_label(question_set, answer))
                })
        })
        .collect::<Vec<_>>()
        .join("; ");
    ledger::append(&ledger::PhaseRecord {
        ts_ms: ledger::now_ms(),
        session_id: meta.session_id.to_string(),
        run_id: meta.run_id.to_string(),
        phase: "plan-question".to_string(),
        cycle: round,
        role: "planner".to_string(),
        provider: role_cfg.provider_name.clone(),
        config_model: role_cfg.model.clone(),
        reported_model: None,
        context_limit: None,
        input_tokens: None,
        output_tokens: None,
        duration_ms: 0,
        verdict: Some(format!("{reason}: {selected}")),
        permission_policy: None,
        permission_denials: None,
        task_preview: safe_truncate(meta.task, 120),
        plan_exemplars_injected: None,
        plan_exemplar_run_ids: None,
        review_exemplars_injected: None,
        review_exemplar_run_ids: None,
        playbook_injected: None,
        arena_rank: None,
        arena_winner: None,
    });
}

fn answer_label(question_set: &orch_ask::OrchQuestionSet, answer: &orch_ask::OrchAnswer) -> String {
    let Some(question) = question_set.questions.get(answer.question_index) else {
        return "(unknown)".to_string();
    };
    match &answer.selection {
        orch_ask::Selection::Option(index) => question
            .options
            .get(*index)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| format!("Option {}", index + 1)),
        orch_ask::Selection::FreeText(text) => format!("Custom: {text}"),
    }
}

#[cfg(test)]
mod tests;
