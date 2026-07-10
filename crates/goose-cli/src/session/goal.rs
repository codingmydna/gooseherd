use anyhow::{Context, Result};
use console::style;
use goose::config::Config;
use goose::conversation::message::Message;
use goose::providers::base::ProviderUsage;
use goose_providers::conversation::token_usage::Usage;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use super::orchestrate::{build_role_provider, resolve_all_roles, RoleConfig};
use super::{ledger, output, CliSession};

pub(crate) const GOAL_USAGE: &str = "\
Usage: /goal <goal text> [--max N] [--check \"shell command\"]
       /goal
       /goal stop
Example: /goal get the homepage Lighthouse score to 90 or above --max 5 --check \"npm run lighthouse\"";

const MAX_ATTEMPTS_KEY: &str = "GOOSE_GOAL_MAX_ATTEMPTS";
const EVALUATOR_PROVIDER_KEY: &str = "GOOSE_EVALUATOR_PROVIDER";
const EVALUATOR_MODEL_KEY: &str = "GOOSE_EVALUATOR_MODEL";
const EVALUATOR_EFFORT_KEY: &str = "GOOSE_EVALUATOR_EFFORT";
const DEFAULT_MAX_ATTEMPTS: u32 = 5;
const CHECK_OUTPUT_TAIL_LIMIT: usize = 4_000;

const EVALUATOR_SYSTEM_PROMPT: &str = r#"You are the evaluator in a goal loop. A worker model has just attempted a user-defined goal. Your session is read-only: judge from the supplied transcript and any allowed read-only context, but do not modify files.

Decide whether the goal is fully met. Prefer concrete evidence over optimism. If the goal is ambiguous, require enough evidence that a careful user would accept the attempt as complete.

Your reply MUST start with exactly one of these lines:
GOAL_MET
GOAL_NOT_MET

After that line, give a brief reason. If not met, include the most useful next correction for the worker."#;

#[derive(Debug, Clone)]
pub(crate) struct GoalCommand {
    pub goal: String,
    pub max_attempts: Option<u32>,
    pub check: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum ParsedGoalCommand {
    Start(GoalCommand),
    Status,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalOutcome {
    Met,
    NotMet,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GoalVerdict {
    Met(String),
    NotMet(String),
    NoVerdict(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GoalCheckOutcome {
    Passed,
    Failed { output_tail: String },
}

#[derive(Debug, Clone)]
pub(super) struct GoalStatusSnapshot {
    pub goal: String,
    pub attempts_used: u32,
    pub max_attempts: u32,
    pub last_verdict: Option<String>,
    pub last_reason: Option<String>,
    pub tokens_spent: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageSnapshot {
    input: i64,
    output: i64,
    total: i64,
}

impl UsageSnapshot {
    fn from_usage(usage: &Usage) -> Self {
        Self {
            input: usage.input_tokens.unwrap_or(0) as i64,
            output: usage.output_tokens.unwrap_or(0) as i64,
            total: usage.total_tokens.unwrap_or(0) as i64,
        }
    }

    fn delta_since(self, before: Self) -> Self {
        Self {
            input: self.input.saturating_sub(before.input),
            output: self.output.saturating_sub(before.output),
            total: self.total.saturating_sub(before.total),
        }
    }
}

struct GoalRunStats {
    started_at: Instant,
    starting_usage: UsageSnapshot,
    evaluator_tokens: i64,
    attempts: u32,
    last_verdict: Option<String>,
    last_reason: Option<String>,
}

struct GoalEvaluatorLedgerRecord<'a> {
    run_id: &'a str,
    attempt: u32,
    goal: &'a str,
    role: Option<&'a RoleConfig>,
    usage: Option<&'a ProviderUsage>,
    duration: Duration,
    verdict: &'a str,
}

impl GoalRunStats {
    fn new(starting_usage: UsageSnapshot) -> Self {
        Self {
            started_at: Instant::now(),
            starting_usage,
            evaluator_tokens: 0,
            attempts: 0,
            last_verdict: None,
            last_reason: None,
        }
    }
}

pub(super) fn parse_goal_verdict(text: &str) -> GoalVerdict {
    // Last verdict line wins (shared verdict protocol): rubric echoes and quoted
    // tokens earlier in the reply lose to the evaluator's closing verdict.
    let Some((index, (met, inline_reason))) =
        crate::session::verdict::last_line_match(text, parse_verdict_line)
    else {
        return GoalVerdict::NoVerdict(default_reason(
            text.trim().to_string(),
            "Evaluator did not return GOAL_MET or GOAL_NOT_MET.",
        ));
    };

    let mut reason_parts = Vec::new();
    if !inline_reason.is_empty() {
        reason_parts.push(inline_reason);
    }
    reason_parts.extend(
        text.lines()
            .skip(index + 1)
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(ToString::to_string),
    );
    let reason = reason_parts.join("\n");
    if met {
        GoalVerdict::Met(default_reason(reason, "Evaluator marked the goal met."))
    } else {
        GoalVerdict::NotMet(default_reason(reason, "Evaluator marked the goal not met."))
    }
}

fn parse_verdict_line(line: &str) -> Option<(bool, String)> {
    let normalized = line
        .trim()
        .strip_prefix("VERDICT:")
        .unwrap_or_else(|| line.trim())
        .trim();

    parse_label_with_reason(normalized, "GOAL_NOT_MET")
        .map(|reason| (false, reason))
        .or_else(|| parse_label_with_reason(normalized, "GOAL_MET").map(|reason| (true, reason)))
}

fn parse_label_with_reason(line: &str, label: &str) -> Option<String> {
    if line == label {
        return Some(String::new());
    }
    let rest = line.strip_prefix(label)?;
    if !rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, ':' | '-'))
    {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(String::new());
    }
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim()
        .to_string();
    Some(rest)
}

fn default_reason(reason: String, fallback: &str) -> String {
    if reason.trim().is_empty() {
        fallback.to_string()
    } else {
        reason
    }
}

pub(super) fn run_goal_check(working_dir: &Path, command: &str) -> GoalCheckOutcome {
    let output = match spawn_shell_check(working_dir, command) {
        Ok(output) => output,
        Err(error) => {
            return GoalCheckOutcome::Failed {
                output_tail: format!("failed to launch check command: {error}"),
            };
        }
    };

    if output.status.success() {
        return GoalCheckOutcome::Passed;
    }

    let mut combined = format!("status: {}\n", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        combined.push_str(&format!("stdout:\n{stdout}\n"));
    }
    if !stderr.trim().is_empty() {
        combined.push_str(&format!("stderr:\n{stderr}\n"));
    }
    GoalCheckOutcome::Failed {
        output_tail: tail_truncate(&combined, CHECK_OUTPUT_TAIL_LIMIT),
    }
}

fn spawn_shell_check(working_dir: &Path, command: &str) -> std::io::Result<std::process::Output> {
    let mut cmd = shell_command(command);
    cmd.current_dir(working_dir).output()
}

#[cfg(unix)]
fn shell_command(command: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

fn tail_truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    let marker = "...";
    let marker_len = marker.chars().count();
    if max_chars <= marker_len {
        return marker.chars().take(max_chars).collect();
    }

    let tail_len = max_chars - marker_len;
    let tail: String = s.chars().skip(count - tail_len).collect();
    format!("{marker}{tail}")
}

pub(super) fn goal_status_label(attempt: u32, max_attempts: u32) -> String {
    format!("goal attempt {attempt}/{max_attempts}")
}

pub(super) fn parse_goal_command_args(
    args: &str,
) -> std::result::Result<ParsedGoalCommand, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(ParsedGoalCommand::Status);
    }
    if args.eq_ignore_ascii_case("stop") {
        return Ok(ParsedGoalCommand::Stop);
    }

    let parts = shlex::split(args).ok_or_else(|| GOAL_USAGE.to_string())?;
    let mut max_attempts = None;
    let mut check = None;
    let mut goal_parts = Vec::new();
    let mut index = 0;

    while index < parts.len() {
        match parts[index].as_str() {
            "--max" => {
                index += 1;
                let Some(raw) = parts.get(index) else {
                    return Err(GOAL_USAGE.to_string());
                };
                let parsed = raw.parse::<u32>().map_err(|_| GOAL_USAGE.to_string())?;
                if parsed == 0 {
                    return Err(GOAL_USAGE.to_string());
                }
                max_attempts = Some(parsed);
                index += 1;
            }
            "--check" => {
                index += 1;
                let Some(raw) = parts.get(index) else {
                    return Err(GOAL_USAGE.to_string());
                };
                if raw.trim().is_empty() {
                    return Err(GOAL_USAGE.to_string());
                }
                check = Some(raw.clone());
                index += 1;
            }
            flag if flag.starts_with("--") => return Err(GOAL_USAGE.to_string()),
            part => {
                goal_parts.push(part.to_string());
                index += 1;
            }
        }
    }

    let goal = goal_parts.join(" ").trim().to_string();
    if goal.is_empty() {
        return Err(GOAL_USAGE.to_string());
    }

    Ok(ParsedGoalCommand::Start(GoalCommand {
        goal,
        max_attempts,
        check,
    }))
}

fn resolved_max_attempts(override_max: Option<u32>) -> u32 {
    override_max
        .filter(|n| *n > 0)
        .or_else(|| {
            Config::global()
                .get_param::<u32>(MAX_ATTEMPTS_KEY)
                .ok()
                .filter(|n| *n > 0)
        })
        .unwrap_or(DEFAULT_MAX_ATTEMPTS)
}

fn goal_run_id() -> String {
    format!("goal-{:x}", ledger::now_ms())
}

fn fmt_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1e6)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1e3)
    } else {
        tokens.to_string()
    }
}

fn provider_usage_total(usage: &ProviderUsage) -> i64 {
    usage
        .usage
        .total_tokens
        .or_else(
            || match (usage.usage.input_tokens, usage.usage.output_tokens) {
                (Some(input), Some(output)) => Some(input + output),
                _ => None,
            },
        )
        .unwrap_or(0) as i64
}

fn combine_provider_usage(first: &ProviderUsage, second: &ProviderUsage) -> ProviderUsage {
    let opt_add = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    ProviderUsage::new(
        first.model.clone(),
        Usage::new(
            opt_add(first.usage.input_tokens, second.usage.input_tokens),
            opt_add(first.usage.output_tokens, second.usage.output_tokens),
            opt_add(first.usage.total_tokens, second.usage.total_tokens),
        ),
    )
}

fn provider_usage_tokens(usage: Option<&ProviderUsage>) -> (Option<i64>, Option<i64>) {
    usage
        .map(|usage| {
            (
                usage.usage.input_tokens.map(|n| n as i64),
                usage.usage.output_tokens.map(|n| n as i64),
            )
        })
        .unwrap_or((None, None))
}

fn resolve_evaluator_role() -> Result<RoleConfig> {
    let config = Config::global();
    let provider = config.get_param::<String>(EVALUATOR_PROVIDER_KEY).ok();
    let model = config.get_param::<String>(EVALUATOR_MODEL_KEY).ok();

    match (provider, model) {
        (Some(provider_name), Some(model)) => Ok(RoleConfig {
            provider_name,
            model,
            effort: config.get_param::<String>(EVALUATOR_EFFORT_KEY).ok(),
        }),
        (None, None) => {
            let roles = resolve_all_roles().map_err(|error| {
                anyhow::anyhow!(
                    "No evaluator model configured. Set {EVALUATOR_PROVIDER_KEY} and {EVALUATOR_MODEL_KEY}, or configure the reviewer fallback with GOOSE_REVIEWER_PROVIDER and GOOSE_REVIEWER_MODEL. Reviewer fallback resolution failed: {error}"
                )
            })?;
            let mut role = roles.reviewer;
            if let Ok(effort) = config.get_param::<String>(EVALUATOR_EFFORT_KEY) {
                role.effort = Some(effort);
            }
            Ok(role)
        }
        _ => anyhow::bail!(
            "Incomplete evaluator config. Set both {EVALUATOR_PROVIDER_KEY} and {EVALUATOR_MODEL_KEY}, or unset both to fall back to GOOSE_REVIEWER_PROVIDER / GOOSE_REVIEWER_MODEL."
        ),
    }
}

fn goal_retry_prompt(goal: &str, feedback: &str) -> String {
    format!(
        "Retry the goal below. The evaluator/check did not accept the previous attempt. Address the feedback directly, then report what changed and how you verified it.\n\nGoal:\n{goal}\n\nFeedback from previous attempt:\n{feedback}"
    )
}

fn evaluator_prompt(goal: &str, attempt: u32, assistant_text: &str, check: Option<&str>) -> String {
    let check_note = match check {
        Some(command) => format!(
            "\nA deterministic check was configured (`{command}`), but this evaluator prompt should only be used when no check is being run."
        ),
        None => String::new(),
    };
    format!(
        "Goal:\n{goal}\n\nAttempt: {attempt}\n\nWorker's latest response:\n{assistant_text}{check_note}\n\nReturn GOAL_MET only if the worker's attempt fully satisfies the goal."
    )
}

impl CliSession {
    pub(crate) async fn headless_goal(&mut self, command: GoalCommand) -> Result<GoalOutcome> {
        let result = self.run_goal(command, false).await;
        self.agent
            .emit_hook(goose::hooks::HookEvent::SessionEnd, &self.session_id)
            .await;
        result
    }

    pub(super) async fn run_goal(
        &mut self,
        command: GoalCommand,
        interactive: bool,
    ) -> Result<GoalOutcome> {
        self.goal_active.store(true, Ordering::SeqCst);
        self.goal_stop_requested.store(false, Ordering::SeqCst);
        let result = self.run_goal_inner(command, interactive).await;
        self.goal_active.store(false, Ordering::SeqCst);
        self.goal_stop_requested.store(false, Ordering::SeqCst);
        self.set_goal_status_line(None);
        self.mark_goal_inactive();
        result
    }

    async fn run_goal_inner(
        &mut self,
        command: GoalCommand,
        interactive: bool,
    ) -> Result<GoalOutcome> {
        let goal = command.goal.trim().to_string();
        if goal.is_empty() {
            output::render_error(GOAL_USAGE);
            return Ok(GoalOutcome::NotMet);
        }

        let max_attempts = resolved_max_attempts(command.max_attempts);
        let run_id = goal_run_id();
        let mut stats = GoalRunStats::new(self.goal_usage_snapshot().await);
        let mut feedback: Option<String> = None;
        let working_dir = self.get_session().await?.working_dir;
        let evaluator_role = if command.check.is_none() {
            Some(resolve_evaluator_role()?)
        } else {
            None
        };

        println!(
            "{}",
            style(format!(
                "goal: {} (max {} attempt{}){}",
                goal,
                max_attempts,
                if max_attempts == 1 { "" } else { "s" },
                command
                    .check
                    .as_ref()
                    .map(|check| format!(" · check `{check}`"))
                    .unwrap_or_default()
            ))
            .dim()
        );

        for attempt in 1..=max_attempts {
            self.set_goal_status_line(Some(goal_status_label(attempt, max_attempts)));
            self.update_goal_status_snapshot(&goal, max_attempts, &stats, true)
                .await;
            self.render_goal_attempt_banner(attempt, max_attempts, feedback.as_deref());

            let before_usage = self.goal_usage_snapshot().await;
            let attempt_started = Instant::now();
            let prompt = feedback
                .as_deref()
                .map(|feedback| goal_retry_prompt(&goal, feedback))
                .unwrap_or_else(|| goal.clone());

            self.push_message(Message::user().with_text(&prompt));
            if interactive {
                output::run_status_hook("thinking");
                output::show_thinking();
            }
            let result = self
                .process_agent_response(interactive, CancellationToken::default())
                .await;
            if interactive {
                output::hide_thinking();
            }

            let after_usage = self.goal_usage_snapshot().await;
            let usage_delta = after_usage.delta_since(before_usage);
            stats.attempts = attempt;

            if let Err(error) = result {
                stats.last_verdict = Some("ERROR".to_string());
                stats.last_reason = Some(error.to_string());
                self.append_goal_attempt_ledger(
                    &run_id,
                    attempt,
                    &goal,
                    usage_delta,
                    attempt_started.elapsed(),
                    Some("ERROR"),
                )
                .await;
                self.render_goal_summary(&stats, "ended after error").await;
                return Err(error);
            }

            let verdict_started = Instant::now();
            let (verdict, evaluator_usage) = self
                .evaluate_goal_attempt(
                    &goal,
                    attempt,
                    command.check.as_deref(),
                    &working_dir,
                    max_attempts,
                    evaluator_role.as_ref(),
                )
                .await?;

            let (verdict_label, reason, met) = match verdict {
                GoalVerdict::Met(reason) => ("GOAL_MET", reason, true),
                GoalVerdict::NotMet(reason) => ("GOAL_NOT_MET", reason, false),
                GoalVerdict::NoVerdict(reason) => ("GOAL_NOT_MET", reason, false),
            };
            if let Some(usage) = evaluator_usage.as_ref() {
                stats.evaluator_tokens += provider_usage_total(usage);
                self.append_goal_evaluator_ledger(GoalEvaluatorLedgerRecord {
                    run_id: &run_id,
                    attempt,
                    goal: &goal,
                    role: evaluator_role.as_ref(),
                    usage: Some(usage),
                    duration: verdict_started.elapsed(),
                    verdict: verdict_label,
                });
            }
            stats.last_verdict = Some(verdict_label.to_string());
            stats.last_reason = Some(reason.clone());
            self.append_goal_attempt_ledger(
                &run_id,
                attempt,
                &goal,
                usage_delta,
                attempt_started.elapsed(),
                Some(verdict_label),
            )
            .await;
            self.update_goal_status_snapshot(&goal, max_attempts, &stats, true)
                .await;
            self.render_goal_verdict(verdict_label, &reason);

            if met {
                self.render_goal_summary(&stats, "met").await;
                return Ok(GoalOutcome::Met);
            }

            if self.goal_stop_requested.swap(false, Ordering::SeqCst) {
                self.render_goal_summary(&stats, "stopped").await;
                return Ok(GoalOutcome::Stopped);
            }

            feedback = Some(format!("{verdict_label}: {reason}"));
        }

        self.render_goal_summary(&stats, "not met").await;
        Ok(GoalOutcome::NotMet)
    }

    async fn evaluate_goal_attempt(
        &self,
        goal: &str,
        attempt: u32,
        check: Option<&str>,
        working_dir: &Path,
        max_attempts: u32,
        evaluator_role: Option<&RoleConfig>,
    ) -> Result<(GoalVerdict, Option<ProviderUsage>)> {
        if let Some(command) = check {
            return Ok(match run_goal_check(working_dir, command) {
                GoalCheckOutcome::Passed => {
                    (GoalVerdict::Met(format!("check passed: {command}")), None)
                }
                GoalCheckOutcome::Failed { output_tail } => (
                    GoalVerdict::NotMet(format!(
                        "check failed: {command}\n\nCheck output (tail):\n{output_tail}"
                    )),
                    None,
                ),
            });
        }

        let role = evaluator_role.context("evaluator role missing")?;
        let prev_plan_explore = Config::global()
            .get_param::<bool>("GOOSE_ACP_PLAN_EXPLORE")
            .ok();
        Config::global().set_param("GOOSE_ACP_PLAN_EXPLORE", true)?;
        let built = build_role_provider(role, working_dir).await;
        match prev_plan_explore {
            Some(value) => Config::global().set_param("GOOSE_ACP_PLAN_EXPLORE", value)?,
            None => Config::global().set_param("GOOSE_ACP_PLAN_EXPLORE", false)?,
        }
        let (provider, model_config) = built.with_context(|| {
            format!(
                "Failed to create evaluator provider {}/{}. Set {EVALUATOR_PROVIDER_KEY} and {EVALUATOR_MODEL_KEY}, or unset them to use the reviewer fallback (GOOSE_REVIEWER_PROVIDER / GOOSE_REVIEWER_MODEL).",
                role.provider_name, role.model
            )
        })?;

        output::set_active_role_status(Some(output::ActiveRoleStatus {
            role: output::ActiveRole::Reviewer,
            cycle: Some((attempt, max_attempts)),
        }));
        output::set_thinking_context(Some(format!(
            "{}/{} evaluating...",
            role.provider_name, role.model
        )));
        let assistant_text = self.goal_last_assistant_text().unwrap_or_default();
        let messages = vec![Message::user().with_text(evaluator_prompt(
            goal,
            attempt,
            &assistant_text,
            check,
        ))];
        let completion = goose::session_context::with_session_id(
            Some(self.session_id.clone()),
            provider.complete(&model_config, EVALUATOR_SYSTEM_PROMPT, &messages, &[]),
        )
        .await
        .with_context(|| {
            format!(
                "Evaluator {}/{} failed. Check {EVALUATOR_PROVIDER_KEY}/{EVALUATOR_MODEL_KEY}, or use --check for a deterministic gate.",
                role.provider_name, role.model
            )
        });
        let (message, usage) = match completion {
            Ok(pair) => pair,
            Err(err) => {
                output::set_active_role_status(None);
                output::set_thinking_context(None);
                return Err(err);
            }
        };
        let verdict = parse_goal_verdict(&message.as_concat_text());
        let (verdict, usage) = if matches!(verdict, GoalVerdict::NoVerdict(_)) {
            self.reprompt_goal_verdict(&provider, &model_config, messages, message, verdict, usage)
                .await
        } else {
            (verdict, usage)
        };
        output::set_active_role_status(None);
        output::set_thinking_context(None);
        Ok((verdict, Some(usage)))
    }

    /// One bounded reprompt when the evaluator omitted its `GOAL_MET` /
    /// `GOAL_NOT_MET` line. Replays the evaluation and the evaluator's reply,
    /// asks for only the verdict line, and keeps the original no-verdict outcome
    /// if that also fails.
    async fn reprompt_goal_verdict(
        &self,
        provider: &std::sync::Arc<dyn goose::providers::base::Provider>,
        model_config: &goose_providers::model::ModelConfig,
        mut messages: Vec<Message>,
        first_reply: Message,
        no_verdict: GoalVerdict,
        first_usage: ProviderUsage,
    ) -> (GoalVerdict, ProviderUsage) {
        messages.push(first_reply);
        messages.push(Message::user().with_text(crate::session::verdict::GOAL_REPROMPT));
        match goose::session_context::with_session_id(
            Some(self.session_id.clone()),
            provider.complete(model_config, EVALUATOR_SYSTEM_PROMPT, &messages, &[]),
        )
        .await
        {
            Ok((message, retry_usage)) => {
                let combined = combine_provider_usage(&first_usage, &retry_usage);
                match parse_goal_verdict(&message.as_concat_text()) {
                    GoalVerdict::NoVerdict(_) => (no_verdict, combined),
                    resolved => (resolved, combined),
                }
            }
            Err(_) => (no_verdict, first_usage),
        }
    }

    pub(super) async fn render_goal_status(&self) -> Result<()> {
        let status = self.goal_status.lock().unwrap().clone();
        let Some(status) = status else {
            println!(
                "\n  {}",
                style("No goal loop recorded yet. Use /goal <goal text> to start one.").dim()
            );
            return Ok(());
        };

        println!();
        println!("{}", style("goal status").bold());
        println!("  goal: {}", status.goal);
        println!(
            "  attempts: {}/{}{}",
            status.attempts_used,
            status.max_attempts,
            if status.active { " (active)" } else { "" }
        );
        println!("  tokens: {}", fmt_tokens(status.tokens_spent));
        if let Some(verdict) = status.last_verdict {
            println!("  verdict: {verdict}");
        }
        if let Some(reason) = status.last_reason {
            println!("  reason: {}", goose::utils::safe_truncate(&reason, 600));
        }
        Ok(())
    }

    fn render_goal_attempt_banner(
        &self,
        attempt: u32,
        max_attempts: u32,
        previous_feedback: Option<&str>,
    ) {
        let mut details = format!("goal attempt {attempt}/{max_attempts}");
        if let Some(feedback) = previous_feedback {
            details.push_str(&format!(
                " · previous: {}",
                goose::utils::safe_truncate(feedback, 120)
            ));
        }
        println!("\n{}", style(details).cyan().bold());
    }

    fn render_goal_verdict(&self, verdict: &str, reason: &str) {
        let color = if verdict == "GOAL_MET" {
            console::Color::Green
        } else {
            console::Color::Yellow
        };
        println!(
            "  {} {}",
            style("⎿").dim(),
            style(format!(
                "{} · {}",
                verdict,
                goose::utils::safe_truncate(reason, 240)
            ))
            .fg(color)
        );
    }

    async fn render_goal_summary(&self, stats: &GoalRunStats, label: &str) {
        let tokens = self
            .goal_usage_snapshot()
            .await
            .delta_since(stats.starting_usage)
            .total
            + stats.evaluator_tokens;
        println!(
            "\n  {}",
            style(format!(
                "goal {} · {} attempt{} · {} elapsed · {} tokens",
                label,
                stats.attempts,
                if stats.attempts == 1 { "" } else { "s" },
                super::format_elapsed_time(stats.started_at.elapsed()),
                fmt_tokens(tokens),
            ))
            .dim()
        );
    }

    fn set_goal_status_line(&self, status: Option<String>) {
        if let Ok(mut cache) = self.completion_cache.write() {
            cache.status_line = status;
        }
    }

    async fn update_goal_status_snapshot(
        &self,
        goal: &str,
        max_attempts: u32,
        stats: &GoalRunStats,
        active: bool,
    ) {
        let tokens_spent = self
            .goal_usage_snapshot()
            .await
            .delta_since(stats.starting_usage)
            .total
            + stats.evaluator_tokens;
        *self.goal_status.lock().unwrap() = Some(GoalStatusSnapshot {
            goal: goal.to_string(),
            attempts_used: stats.attempts,
            max_attempts,
            last_verdict: stats.last_verdict.clone(),
            last_reason: stats.last_reason.clone(),
            tokens_spent,
            active,
        });
    }

    fn mark_goal_inactive(&self) {
        if let Some(status) = self.goal_status.lock().unwrap().as_mut() {
            status.active = false;
        }
    }

    async fn goal_usage_snapshot(&self) -> UsageSnapshot {
        self.get_session()
            .await
            .map(|session| UsageSnapshot::from_usage(&session.accumulated_usage))
            .unwrap_or_default()
    }

    fn goal_last_assistant_text(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == rmcp::model::Role::Assistant)
            .map(|message| message.as_concat_text())
    }

    async fn append_goal_attempt_ledger(
        &self,
        run_id: &str,
        attempt: u32,
        goal: &str,
        usage_delta: UsageSnapshot,
        duration: Duration,
        verdict: Option<&str>,
    ) {
        let provider = match self.agent.provider().await {
            Ok(provider) => provider,
            Err(_) => return,
        };
        let model_config = match self.agent.model_config_for_session(&self.session_id).await {
            Ok(config) => config,
            Err(_) => return,
        };
        let context_limit = provider
            .get_context_limit(&model_config)
            .await
            .ok()
            .or_else(|| Some(model_config.context_limit()));

        ledger::append(&ledger::PhaseRecord {
            ts_ms: ledger::now_ms(),
            session_id: self.session_id.clone(),
            run_id: run_id.to_string(),
            phase: "goal".to_string(),
            cycle: attempt,
            role: "session".to_string(),
            provider: provider.get_name().to_string(),
            config_model: model_config.model_name,
            reported_model: None,
            context_limit,
            input_tokens: Some(usage_delta.input),
            output_tokens: Some(usage_delta.output),
            duration_ms: duration.as_millis() as u64,
            verdict: verdict.map(ToString::to_string),
            permission_policy: None,
            permission_denials: None,
            task_preview: goose::utils::safe_truncate(goal, 160),
            plan_exemplars_injected: None,
            plan_exemplar_run_ids: None,
            review_exemplars_injected: None,
            review_exemplar_run_ids: None,
        });
    }

    fn append_goal_evaluator_ledger(&self, record: GoalEvaluatorLedgerRecord<'_>) {
        let Some(role) = record.role else {
            return;
        };
        let (input_tokens, output_tokens) = provider_usage_tokens(record.usage);
        ledger::append(&ledger::PhaseRecord {
            ts_ms: ledger::now_ms(),
            session_id: self.session_id.clone(),
            run_id: record.run_id.to_string(),
            phase: "goal-eval".to_string(),
            cycle: record.attempt,
            role: "evaluator".to_string(),
            provider: role.provider_name.clone(),
            config_model: role.model.clone(),
            reported_model: record.usage.map(|usage| usage.model.clone()),
            context_limit: None,
            input_tokens,
            output_tokens,
            duration_ms: record.duration.as_millis() as u64,
            verdict: Some(record.verdict.to_string()),
            permission_policy: None,
            permission_denials: None,
            task_preview: goose::utils::safe_truncate(record.goal, 160),
            plan_exemplars_injected: None,
            plan_exemplar_run_ids: None,
            review_exemplars_injected: None,
            review_exemplar_run_ids: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn parse_goal_verdict_extracts_met_reason() {
        let verdict =
            super::parse_goal_verdict("GOAL_MET\nThe deterministic target has been reached.");

        assert_eq!(
            verdict,
            super::GoalVerdict::Met("The deterministic target has been reached.".to_string())
        );
    }

    #[test]
    fn parse_goal_verdict_extracts_not_met_reason_from_verdict_line() {
        let verdict = super::parse_goal_verdict(
            "Notes before verdict\nVERDICT: GOAL_NOT_MET\nMissing regression coverage.",
        );

        assert_eq!(
            verdict,
            super::GoalVerdict::NotMet("Missing regression coverage.".to_string())
        );
    }

    #[test]
    fn parse_goal_verdict_last_verdict_line_wins() {
        // An early quoted verdict must lose to the evaluator's closing verdict.
        let verdict = super::parse_goal_verdict(
            "GOAL_MET looked plausible at first\nAfter re-checking:\nGOAL_NOT_MET\nThe check still fails.",
        );

        assert_eq!(
            verdict,
            super::GoalVerdict::NotMet("The check still fails.".to_string())
        );
    }

    #[test]
    fn parse_goal_verdict_without_marker_is_no_verdict() {
        assert!(matches!(
            super::parse_goal_verdict("The goal looks done to me, nice work."),
            super::GoalVerdict::NoVerdict(_)
        ));
    }

    #[test]
    fn run_goal_check_passes_on_zero_exit_in_working_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("sentinel"), "present\n").expect("write sentinel");

        assert_eq!(
            super::run_goal_check(temp.path(), "test -f sentinel"),
            super::GoalCheckOutcome::Passed
        );
    }

    #[test]
    fn run_goal_check_fails_with_output_tail_on_nonzero_exit() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_goal_check(temp.path(), "echo GOAL_CHECK_MARKER 1>&2; exit 7") {
            super::GoalCheckOutcome::Failed { output_tail } => {
                assert!(
                    output_tail.contains("status: exit status: 7"),
                    "{output_tail}"
                );
                assert!(output_tail.contains("GOAL_CHECK_MARKER"), "{output_tail}");
            }
            super::GoalCheckOutcome::Passed => panic!("expected failing check"),
        }
    }

    #[test]
    fn goal_status_label_formats_attempt_progress() {
        assert_eq!(super::goal_status_label(2, 5), "goal attempt 2/5");
    }

    #[test]
    fn parse_goal_command_accepts_max_and_check_after_goal() {
        match super::parse_goal_command_args(
            "make tests pass --max 3 --check \"cargo test -p goose-cli\"",
        )
        .expect("parse goal")
        {
            super::ParsedGoalCommand::Start(command) => {
                assert_eq!(command.goal, "make tests pass");
                assert_eq!(command.max_attempts, Some(3));
                assert_eq!(command.check.as_deref(), Some("cargo test -p goose-cli"));
            }
            other => panic!("expected start, got {other:?}"),
        }
    }

    #[test]
    fn parse_goal_command_status_and_stop() {
        assert!(matches!(
            super::parse_goal_command_args("").expect("status"),
            super::ParsedGoalCommand::Status
        ));
        assert!(matches!(
            super::parse_goal_command_args("stop").expect("stop"),
            super::ParsedGoalCommand::Stop
        ));
    }
}
