use anyhow::Result;
use goose::config::{Config, GooseMode};
use goose::conversation::message::Message;
use goose::providers::base::{Provider, ProviderUsage};
use goose::utils::safe_truncate;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{ledger, output, CliSession};

const MAX_CYCLES_KEY: &str = "GOOSE_ORCH_MAX_CYCLES";
const DEFAULT_MAX_CYCLES: u32 = 3;
const EVIDENCE_CHAR_LIMIT: usize = 30_000;

const PLAN_SYSTEM_PROMPT: &str = r#"You are the planning lead in a two-model workflow. A separate implementer model will execute your plan with file-editing and shell tools. Your session is read-only: you can explore the working directory but cannot modify anything.

Produce a concrete, step-by-step implementation plan for the given task:
- Explore freely: read files, search, and delegate read-only subagent explorations (in parallel when useful). File modifications will be denied by policy; shell commands are denied unless the session allows them — do not retry denied calls.
- List the files to create or modify and what changes each needs.
- Define acceptance criteria and how the implementer should verify the result (commands to run, expected output).
- Keep the plan focused; do not attempt to implement the changes yourself.
- Even if some exploration is blocked, always deliver your best plan from what you could read.

Output only the plan."#;

const REVIEW_SYSTEM_PROMPT: &str = r#"You are the reviewing lead in a two-model workflow. An implementer model has just attempted the task. You receive the original task, the plan, the git evidence of what changed, and the implementer's report. Your session is read-only: you can inspect files in the working directory but cannot modify anything.

Judge whether the implementation correctly and completely fulfills the task and plan. Inspect files in the working directory if the evidence is insufficient. Some tool calls (especially shell commands) may be denied by policy; do not retry them — judge from file reads and the provided evidence instead. You must always deliver a verdict.

Your reply MUST start with exactly one of these lines:
VERDICT: APPROVED
VERDICT: REVISE

If REVISE, follow with a numbered list of concrete, actionable defects (file, problem, required fix). Only demand changes for real problems; do not invent nitpicks."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchOutcome {
    Approved,
    MaxCycles,
    Aborted,
}

#[derive(Clone, PartialEq)]
pub(super) struct RoleConfig {
    pub(super) provider_name: String,
    pub(super) model: String,
    pub(super) effort: Option<String>,
}

pub(super) struct OrchRoles {
    pub(super) default: RoleConfig,
    pub(super) planner: RoleConfig,
    pub(super) reviewer: RoleConfig,
    pub(super) implementer: RoleConfig,
}

pub(super) fn resolve_all_roles() -> Result<OrchRoles> {
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

pub(super) async fn build_role_provider(
    role: &RoleConfig,
) -> Result<(Arc<dyn Provider>, goose_providers::model::ModelConfig)> {
    let config = Config::global();
    let mut model_config = goose::model_config::model_config_from_user_config(
        &role.provider_name,
        role.model.as_str(),
    )?;
    // Per-role reasoning effort. API providers that don't support it simply
    // ignore the setting (e.g. local models); ACP agents manage their own.
    if let Some(effort) = role
        .effort
        .as_deref()
        .and_then(|e| e.parse::<goose_providers::thinking::ThinkingEffort>().ok())
    {
        model_config = model_config.with_thinking_effort(effort);
    }
    let extensions = goose::config::extensions::get_enabled_extensions_with_config(config);
    let provider = goose::providers::create(&role.provider_name, extensions).await?;
    Ok((provider, model_config))
}

struct Evidence {
    text: String,
    full: String,
    truncated: bool,
}

fn git_evidence() -> Evidence {
    let mut evidence = String::new();
    for args in [
        &["status", "--short"][..],
        &["diff", "HEAD"][..],
        &["diff", "--cached"][..],
    ] {
        if let Ok(out) = std::process::Command::new("git").args(args).output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if !text.trim().is_empty() {
                    evidence.push_str(&format!("$ git {}\n{}\n", args.join(" "), text));
                }
            }
        }
    }
    if evidence.is_empty() {
        let text =
            "No git changes detected (not a git repository, or working tree clean).".to_string();
        return Evidence {
            full: text.clone(),
            text,
            truncated: false,
        };
    }
    let text = safe_truncate(&evidence, EVIDENCE_CHAR_LIMIT);
    let truncated = text.len() < evidence.len();
    Evidence {
        text,
        full: evidence,
        truncated,
    }
}

fn parse_verdict_approved(review: &str) -> bool {
    review
        .lines()
        .find(|l| l.trim_start().starts_with("VERDICT:"))
        .map(|l| l.contains("APPROVED"))
        .unwrap_or(false)
}

fn phase_banner(text: &str, role: output::ActiveRole) {
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

/// Distilled Fable 5 operating procedure, injected into roles served by bare
/// API/local providers. ACP agents ship their own co-trained harness and
/// don't need it.
const FABLE5_PLAYBOOK: &str = include_str!("../../../../profiles/fable5-playbook.md");

/// The embedded playbook, or the file GOOSE_PLAYBOOK_PATH points at —
/// letting users refine their own playbook without forking the repo.
fn playbook_text() -> String {
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

fn is_acp_provider(provider_name: &str) -> bool {
    provider_name.ends_with("-acp")
}

fn role_system_prompt(base: &str, role: &RoleConfig) -> String {
    if is_acp_provider(&role.provider_name) {
        base.to_string()
    } else {
        format!("{}\n\n# Operating playbook\n\n{}", base, playbook_text())
    }
}

/// Write an orchestration artifact under <working_dir>/.goose-orch/<run_id>/.
fn persist_artifact(working_dir: &str, run_id: &str, name: &str, content: &str) {
    let dir = std::path::Path::new(working_dir)
        .join(".goose-orch")
        .join(run_id);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(name), content);
    }
}

fn warn_truncated(what: &str, full_len: usize, run_id: &str) {
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

/// Run a role completion with live streaming render (tool calls, thinking,
/// text appear as they happen — same visibility as the main agent loop),
/// returning the concatenated assistant text and the final usage report.
async fn stream_role_completion(
    provider: &Arc<dyn Provider>,
    model_config: &goose_providers::model::ModelConfig,
    system: &str,
    request: Message,
    session_id: &str,
    debug: bool,
) -> Result<(String, Option<ProviderUsage>)> {
    use futures::StreamExt;

    let mut stream = goose::session_context::with_session_id(
        Some(session_id.to_string()),
        provider.stream(model_config, system, &[request], &[]),
    )
    .await?;

    let mut buffer = super::streaming_buffer::MarkdownBuffer::new();
    let mut thinking_header_shown = false;
    let mut text = String::new();
    let mut usage: Option<ProviderUsage> = None;

    while let Some(next) = stream.next().await {
        let (message, message_usage) = next?;
        if let Some(message) = message {
            for content in &message.content {
                if let goose::conversation::message::MessageContent::Text(t) = content {
                    text.push_str(&t.text);
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
    }
    output::flush_markdown_buffer_current_theme(&mut buffer);
    output::reset_response_bullet();
    Ok((text, usage))
}

struct PhaseMeta<'a> {
    session_id: &'a str,
    run_id: &'a str,
    task: &'a str,
}

/// Print a phase summary line, warn when the reported model doesn't match
/// GOOSE_<ROLE>_EXPECT_MODEL, and append the phase to the run ledger.
#[allow(clippy::too_many_arguments)]
fn record_phase(
    meta: &PhaseMeta<'_>,
    phase: &str,
    cycle: u32,
    role: &str,
    role_cfg: &RoleConfig,
    usage: Option<&ProviderUsage>,
    context_limit: Option<usize>,
    elapsed_ms: u64,
    verdict: Option<&str>,
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
    println!(
        "  {} {}",
        console::style("⎿").dim(),
        console::style(format!(
            "{} done · model {} · in {} / out {} · {:.1}s{}",
            phase,
            reported_model.as_deref().unwrap_or("(unreported)"),
            fmt_tok(input_tokens),
            fmt_tok(output_tokens),
            elapsed_ms as f64 / 1000.0,
            verdict.map(|v| format!(" · {}", v)).unwrap_or_default()
        ))
        .dim()
    );

    if let Ok(expected) =
        Config::global().get_param::<String>(&format!("GOOSE_{}_EXPECT_MODEL", role.to_uppercase()))
    {
        let generic = match reported_model.as_deref().map(|m| m.to_lowercase()) {
            None => true,
            Some(m) => m.is_empty() || m == "default" || m == "current" || m == "unknown",
        };
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
        task_preview: safe_truncate(meta.task, 120),
    });
}

impl CliSession {
    pub(crate) async fn handle_orchestrate(
        &mut self,
        task: String,
        max_cycles_override: Option<u32>,
        interactive: bool,
    ) -> Result<OrchOutcome> {
        let task = task.trim().to_string();
        if task.is_empty() {
            output::render_error(
                "Usage: /orch <task> — plan with the planner model, implement with the implementer model, review with the reviewer model until approved.",
            );
            return Ok(OrchOutcome::Aborted);
        }

        let config = Config::global();
        let roles = resolve_all_roles()?;
        let default_role = roles.default;
        let planner_role = roles.planner;
        let reviewer_role = roles.reviewer;
        let implementer_role = roles.implementer;
        let max_cycles = max_cycles_override
            .filter(|n| *n >= 1)
            .or_else(|| {
                config
                    .get_param::<u32>(MAX_CYCLES_KEY)
                    .ok()
                    .filter(|n| *n >= 1)
            })
            .unwrap_or(DEFAULT_MAX_CYCLES);

        println!(
            "{}",
            console::style(format!(
                "orchestrate: planner={}/{} implementer={}/{} reviewer={}/{} (max {} cycles)",
                planner_role.provider_name,
                planner_role.model,
                implementer_role.provider_name,
                implementer_role.model,
                reviewer_role.provider_name,
                reviewer_role.model,
                max_cycles
            ))
            .dim()
        );

        let prev_mode = config.get_goose_mode().unwrap_or_default();
        let outcome = self
            .run_orchestration(
                &task,
                &planner_role,
                &reviewer_role,
                &implementer_role,
                max_cycles,
                interactive,
            )
            .await;

        // Restore the session provider and goose mode no matter how the run ended.
        // Plan-Explore must never leak into subsequent provider creations.
        output::set_active_role(None);
        output::set_thinking_context(None);
        if let Err(e) = config.set_param("GOOSE_ACP_PLAN_EXPLORE", false) {
            output::render_error(&format!("Failed to reset plan-explore flag: {}", e));
        }
        if let Ok(restore_model_config) = goose::model_config::model_config_from_user_config(
            &default_role.provider_name,
            default_role.model.as_str(),
        ) {
            if let Err(e) = self
                .agent
                .recreate_provider_for_session(
                    &self.session_id,
                    &default_role.provider_name,
                    restore_model_config,
                )
                .await
            {
                output::render_error(&format!("Failed to restore session provider: {}", e));
            }
        }
        if let Err(e) = config.set_goose_mode(prev_mode) {
            output::render_error(&format!("Failed to restore goose mode: {}", e));
        }

        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_orchestration(
        &mut self,
        task: &str,
        planner_role: &RoleConfig,
        reviewer_role: &RoleConfig,
        implementer_role: &RoleConfig,
        max_cycles: u32,
        interactive: bool,
    ) -> Result<OrchOutcome> {
        let config = Config::global();
        let working_dir = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Planner and reviewer can be full agents (ACP providers). Plan-Explore
        // gives them full read-only exploration (file reads, search, subagent
        // delegation) while goose's kind-based permission policy rejects every
        // mutating tool call — provider-agnostic, no reliance on the agent's
        // own mode semantics.
        config.set_param("GOOSE_ACP_PLAN_EXPLORE", true)?;

        let run_id = format!("{:x}", ledger::now_ms());
        let session_id = self.session_id.clone();
        let meta = PhaseMeta {
            session_id: &session_id,
            run_id: &run_id,
            task,
        };

        output::set_active_role(Some(output::ActiveRole::Planner));
        phase_banner(
            &format!(
                "phase: plan · {}/{}",
                planner_role.provider_name, planner_role.model
            ),
            output::ActiveRole::Planner,
        );
        output::set_thinking_context(Some(format!(
            "planner {}/{} working…",
            planner_role.provider_name, planner_role.model
        )));
        let phase_started = Instant::now();
        let (planner, planner_model) = build_role_provider(planner_role).await?;
        output::show_thinking();
        // ACP providers wrap full agent CLIs that may not receive our system prompt,
        // so the role instructions are embedded in the user message as well.
        let plan_request = Message::user().with_text(format!(
            "{}\n\n---\n\nTask:\n{}\n\nWorking directory: {}",
            PLAN_SYSTEM_PROMPT, task, working_dir
        ));
        let planner_system = role_system_prompt(PLAN_SYSTEM_PROMPT, planner_role);
        let (plan_text, plan_usage) = stream_role_completion(
            &planner,
            &planner_model,
            &planner_system,
            plan_request,
            &self.session_id,
            self.debug,
        )
        .await?;
        output::hide_thinking();
        // Captured after the completion so the ACP adapter has reported the
        // session's real context size (a model fingerprint).
        let planner_context_limit = planner.get_context_limit(&planner_model).await.ok();
        record_phase(
            &meta,
            "plan",
            0,
            "planner",
            planner_role,
            plan_usage.as_ref(),
            planner_context_limit,
            phase_started.elapsed().as_millis() as u64,
            None,
        );

        persist_artifact(&working_dir, &run_id, "plan.md", &plan_text);
        println!(
            "  {}",
            console::style(format!("artifacts → .goose-orch/{}/", run_id)).dim()
        );

        let (reviewer, reviewer_model) = if reviewer_role == planner_role {
            (Arc::clone(&planner), planner_model.clone())
        } else {
            build_role_provider(reviewer_role).await?
        };
        config.set_param("GOOSE_ACP_PLAN_EXPLORE", false)?;

        // The implementer session needs to act without approval prompts.
        config.set_goose_mode(GooseMode::Auto)?;
        let impl_model_config = goose::model_config::model_config_from_user_config(
            &implementer_role.provider_name,
            implementer_role.model.as_str(),
        )?;
        self.agent
            .recreate_provider_for_session(
                &self.session_id,
                &implementer_role.provider_name,
                impl_model_config,
            )
            .await?;

        let implementer_playbook = if is_acp_provider(&implementer_role.provider_name) {
            String::new()
        } else {
            format!("\n\n# Operating playbook\n\n{}", playbook_text())
        };
        let mut instruction = format!(
            "You are the implementer in a plan/implement/review workflow. Execute the plan below for the task. Modify files and run verification with your tools. When done, report what you changed and how you verified it.{}\n\nTask:\n{}\n\nPlan:\n{}",
            implementer_playbook, task, plan_text
        );

        for cycle in 1..=max_cycles {
            output::set_active_role(Some(output::ActiveRole::Implementer));
            phase_banner(
                &format!(
                    "phase: implement (cycle {}/{}) · {}/{}",
                    cycle, max_cycles, implementer_role.provider_name, implementer_role.model
                ),
                output::ActiveRole::Implementer,
            );
            output::set_thinking_context(Some(format!(
                "implementer {}/{} working…",
                implementer_role.provider_name, implementer_role.model
            )));
            let phase_started = Instant::now();
            let usage_before = self
                .get_session()
                .await
                .map(|s| s.accumulated_usage)
                .unwrap_or_default();
            self.push_message(Message::user().with_text(&instruction));
            output::show_thinking();
            self.process_agent_response(interactive, CancellationToken::default())
                .await?;
            output::hide_thinking();
            let usage_after = self
                .get_session()
                .await
                .map(|s| s.accumulated_usage)
                .unwrap_or_default();
            let delta = |after: Option<i32>, before: Option<i32>| match (after, before) {
                (Some(a), Some(b)) => Some((a - b) as i64),
                (Some(a), None) => Some(a as i64),
                _ => None,
            };
            let impl_usage = ProviderUsage::new(
                implementer_role.model.clone(),
                goose::providers::base::Usage {
                    input_tokens: delta(usage_after.input_tokens, usage_before.input_tokens)
                        .map(|n| n as i32),
                    output_tokens: delta(usage_after.output_tokens, usage_before.output_tokens)
                        .map(|n| n as i32),
                    total_tokens: None,
                    cache_read_input_tokens: None,
                    cache_write_input_tokens: None,
                },
            );
            record_phase(
                &meta,
                "implement",
                cycle,
                "implementer",
                implementer_role,
                Some(&impl_usage),
                None,
                phase_started.elapsed().as_millis() as u64,
                None,
            );

            output::set_active_role(Some(output::ActiveRole::Reviewer));
            phase_banner(
                &format!(
                    "phase: review (cycle {}/{}) · {}/{}",
                    cycle, max_cycles, reviewer_role.provider_name, reviewer_role.model
                ),
                output::ActiveRole::Reviewer,
            );
            output::set_thinking_context(Some(format!(
                "reviewer {}/{} working…",
                reviewer_role.provider_name, reviewer_role.model
            )));
            let phase_started = Instant::now();
            let implementer_report = self
                .messages
                .messages()
                .iter()
                .rev()
                .find(|m| m.role == rmcp::model::Role::Assistant)
                .map(|m| m.as_concat_text())
                .unwrap_or_default();
            persist_artifact(
                &working_dir,
                &run_id,
                &format!("report-c{}.md", cycle),
                &implementer_report,
            );
            if implementer_report.len() > EVIDENCE_CHAR_LIMIT {
                warn_truncated("implementer report", implementer_report.len(), &run_id);
            }
            let evidence = git_evidence();
            persist_artifact(
                &working_dir,
                &run_id,
                &format!("evidence-c{}.diff", cycle),
                &evidence.full,
            );
            if evidence.truncated {
                warn_truncated("git evidence", evidence.full.len(), &run_id);
            }
            let review_request = Message::user().with_text(format!(
                "{}\n\n---\n\nTask:\n{}\n\nPlan:\n{}\n\nGit evidence:\n{}\n\nImplementer report:\n{}\n\nWorking directory: {}",
                REVIEW_SYSTEM_PROMPT,
                task,
                plan_text,
                evidence.text,
                safe_truncate(&implementer_report, EVIDENCE_CHAR_LIMIT),
                working_dir
            ));
            output::show_thinking();
            let reviewer_system = role_system_prompt(REVIEW_SYSTEM_PROMPT, reviewer_role);
            let (review_text, review_usage) = stream_role_completion(
                &reviewer,
                &reviewer_model,
                &reviewer_system,
                review_request,
                &self.session_id,
                self.debug,
            )
            .await?;
            output::hide_thinking();
            persist_artifact(
                &working_dir,
                &run_id,
                &format!("review-c{}.md", cycle),
                &review_text,
            );
            let approved = parse_verdict_approved(&review_text);
            let verdict = if approved {
                "APPROVED"
            } else if review_text.contains("VERDICT:") {
                "REVISE"
            } else {
                "NO_VERDICT"
            };
            record_phase(
                &meta,
                "review",
                cycle,
                "reviewer",
                reviewer_role,
                review_usage.as_ref(),
                None,
                phase_started.elapsed().as_millis() as u64,
                Some(verdict),
            );

            if approved {
                println!(
                    "{}",
                    console::style("orchestrate: reviewer approved the implementation.")
                        .green()
                        .bold()
                );
                return Ok(OrchOutcome::Approved);
            }
            if cycle == max_cycles {
                println!(
                    "{}",
                    console::style(format!(
                        "orchestrate: max cycles ({}) reached without approval. Last review feedback is above.",
                        max_cycles
                    ))
                    .yellow()
                    .bold()
                );
                return Ok(OrchOutcome::MaxCycles);
            }
            instruction = format!(
                "The reviewer did not approve the implementation. Address every item in the review feedback below, then re-verify and report.\n\nReview feedback:\n{}",
                review_text
            );
        }
        Ok(OrchOutcome::MaxCycles)
    }
}
