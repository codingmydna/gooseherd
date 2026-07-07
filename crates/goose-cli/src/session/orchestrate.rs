use crate::worktree::{self, CreatedWorktree, EnvLink, MergeResult};
use anyhow::{Context, Result};
use goose::config::{Config, GooseMode};
use goose::conversation::message::Message;
use goose::providers::base::{Provider, ProviderUsage};
use goose::utils::safe_truncate;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use super::{ledger, output, CliSession};

const MAX_CYCLES_KEY: &str = "GOOSE_ORCH_MAX_CYCLES";
const PHASE_IDLE_TIMEOUT_KEY: &str = "GOOSE_ORCH_PHASE_IDLE_TIMEOUT_SECS";
const DEFAULT_MAX_CYCLES: u32 = 3;
const DEFAULT_PHASE_IDLE_TIMEOUT_SECS: u64 = 120;
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
    working_dir: &Path,
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
    let provider = goose::providers::create_with_working_dir(
        &role.provider_name,
        extensions,
        working_dir.into(),
    )
    .await?;
    Ok((provider, model_config))
}

struct Evidence {
    text: String,
    full: String,
    truncated: bool,
}

struct OrchWorkspace {
    original_dir: PathBuf,
    impl_dir: PathBuf,
    repo_root: Option<PathBuf>,
    branch: Option<String>,
    env_links: Vec<EnvLink>,
    in_place_reason: Option<String>,
}

impl OrchWorkspace {
    fn in_place(original_dir: PathBuf, reason: impl Into<String>) -> Self {
        Self {
            impl_dir: original_dir.clone(),
            original_dir,
            repo_root: None,
            branch: None,
            env_links: Vec::new(),
            in_place_reason: Some(reason.into()),
        }
    }

    fn worktree(original_dir: PathBuf, repo_root: PathBuf, created: CreatedWorktree) -> Self {
        Self {
            original_dir,
            impl_dir: created.path,
            repo_root: Some(repo_root),
            branch: Some(created.branch),
            env_links: created.env_links,
            in_place_reason: None,
        }
    }

    fn is_worktree(&self) -> bool {
        self.branch.is_some()
    }
}

fn setup_orch_workspace(original_dir: &Path, run_id: &str) -> OrchWorkspace {
    let config = Config::global();
    let force_in_place = config
        .get_param::<bool>("GOOSE_ORCH_IN_PLACE")
        .unwrap_or(false);
    setup_orch_workspace_with_force(original_dir, run_id, force_in_place)
}

fn setup_orch_workspace_with_force(
    original_dir: &Path,
    run_id: &str,
    force_in_place: bool,
) -> OrchWorkspace {
    if force_in_place {
        return OrchWorkspace::in_place(original_dir.to_path_buf(), "GOOSE_ORCH_IN_PLACE=true");
    }

    let repo_root = match worktree::find_repo_root(original_dir) {
        Ok(repo_root) => repo_root,
        Err(_) => {
            return OrchWorkspace::in_place(original_dir.to_path_buf(), "not a git repository");
        }
    };

    let name = format!("orch-{run_id}");
    let branch = format!("orch/{run_id}");
    match worktree::create_named_worktree(original_dir, &name, Some(&branch)) {
        Ok(created) => OrchWorkspace::worktree(original_dir.to_path_buf(), repo_root, created),
        Err(error) => OrchWorkspace::in_place(
            original_dir.to_path_buf(),
            format!("worktree creation failed: {error}"),
        ),
    }
}

fn display_workspace_path(workspace: &OrchWorkspace) -> String {
    workspace
        .repo_root
        .as_ref()
        .and_then(|repo_root| workspace.impl_dir.strip_prefix(repo_root).ok())
        .unwrap_or(&workspace.impl_dir)
        .display()
        .to_string()
}

fn render_workspace_banner(workspace: &OrchWorkspace, auto_merge: bool) {
    if let Some(branch) = workspace.branch.as_deref() {
        println!(
            "{}",
            console::style(format!(
                "orchestrate workspace: worktree {} · branch {}{}",
                display_workspace_path(workspace),
                branch,
                if auto_merge {
                    " · auto-merge enabled"
                } else {
                    ""
                }
            ))
            .dim()
        );
        if !workspace.env_links.is_empty() {
            let linked = workspace
                .env_links
                .iter()
                .filter_map(|link| link.destination.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  {}",
                console::style(format!("linked env files: {linked}")).dim()
            );
        }
    } else {
        println!(
            "{}",
            console::style(format!(
                "orchestrate workspace: in-place: {} (no worktree - auto-commit/merge disabled)",
                workspace
                    .in_place_reason
                    .as_deref()
                    .unwrap_or("unknown reason")
            ))
            .yellow()
        );
    }
}

fn git_evidence(dir: &Path) -> Evidence {
    let mut evidence = String::new();
    for args in [
        &["status", "--short"][..],
        &["diff", "HEAD"][..],
        &["diff", "--cached"][..],
    ] {
        if let Ok(out) = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
        {
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

fn changed_paths_for_commit(dir: &Path) -> Vec<String> {
    worktree::git(
        dir,
        &[
            "-c",
            "core.quotePath=off",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    )
    .map(|status| {
        status
            .lines()
            .filter_map(parse_status_path)
            .filter(|path| path != ".goose-orch" && !path.starts_with(".goose-orch/"))
            .collect()
    })
    .unwrap_or_default()
}

fn parse_status_path(line: &str) -> Option<String> {
    let mut chars = line.chars();
    for _ in 0..3 {
        chars.next()?;
    }
    let path = chars.as_str();
    let path = path
        .split_once(" -> ")
        .map(|(_, renamed)| renamed)
        .unwrap_or(path);
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn conventional_commit_subject(task: &str, changed_paths: &[String]) -> String {
    const SUMMARY_LIMIT: usize = 65;

    let task_lower = task.to_lowercase();
    let kind = if task_lower.contains("fix")
        || task_lower.contains("bug")
        || task_lower.contains("버그")
        || task_lower.contains("수정")
    {
        "fix"
    } else if task_lower.contains("doc") || task_lower.contains("문서") {
        "docs"
    } else if task_lower.contains("test") {
        "test"
    } else if task_lower.contains("refactor") || task_lower.contains("리팩") {
        "refactor"
    } else {
        "feat"
    };

    let summary = task
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("update orch lifecycle")
        .trim_end_matches('.')
        .trim();
    let summary = lowercase_first(summary);
    let summary = safe_truncate(&summary, SUMMARY_LIMIT);

    match common_scope(changed_paths) {
        Some(scope) => format!("{kind}({scope}): {summary}"),
        None => format!("{kind}: {summary}"),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return "update orch lifecycle".to_string();
    };
    first.to_lowercase().collect::<String>() + chars.as_str()
}

fn common_scope(paths: &[String]) -> Option<String> {
    let mut scopes = paths
        .iter()
        .filter_map(|path| path_scope(path))
        .filter(|scope| !scope.is_empty());
    let first = scopes.next()?;
    if scopes.all(|scope| scope == first) {
        Some(first)
    } else {
        None
    }
}

fn path_scope(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches("./");
    if path.starts_with("crates/goose-cli/") {
        Some("cli".to_string())
    } else if path.starts_with("crates/goose-mcp/") {
        Some("mcp".to_string())
    } else if path.starts_with("crates/goose-server/") {
        Some("server".to_string())
    } else if path.starts_with("crates/goose/") {
        Some("goose".to_string())
    } else if path.starts_with("ui/desktop/") {
        Some("ui".to_string())
    } else {
        path.split('/').next().map(ToString::to_string)
    }
}

fn finalize_worktree_approval(workspace: &OrchWorkspace, task: &str, auto_merge: bool) {
    let Some(branch) = workspace.branch.as_deref() else {
        println!(
            "  {}",
            console::style("auto-commit skipped: in-place orchestration").dim()
        );
        return;
    };

    let changed_paths = changed_paths_for_commit(&workspace.impl_dir);
    let message = conventional_commit_subject(task, &changed_paths);
    match worktree::commit_all(&workspace.impl_dir, &message, &[".goose-orch"]) {
        Ok(true) => {
            println!(
                "{}",
                console::style(format!("orchestrate: committed {branch}: {message}"))
                    .green()
                    .bold()
            );
            println!("  병합하려면: git merge {branch}");
        }
        Ok(false) => {
            println!(
                "  {}",
                console::style("auto-commit skipped: no changes to commit").dim()
            );
            return;
        }
        Err(error) => {
            output::render_error(&format!("Auto-commit failed: {error}"));
            return;
        }
    }

    if !auto_merge {
        return;
    }

    match worktree::merge_branch(&workspace.original_dir, branch) {
        Ok(MergeResult::Merged) => {
            println!(
                "{}",
                console::style(format!(
                    "orchestrate: merged {branch} into the original branch."
                ))
                .green()
                .bold()
            );
            if let Some(repo_root) = workspace.repo_root.as_deref() {
                if let Err(error) = worktree::remove_worktree(repo_root, &workspace.impl_dir, true)
                {
                    output::render_error(&format!(
                        "Merged, but failed to remove worktree {}: {}",
                        workspace.impl_dir.display(),
                        error
                    ));
                }
            }
        }
        Ok(MergeResult::Conflict) => {
            output::render_error(&format!(
                "Auto-merge stopped because of conflicts. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
        }
        Err(error) => {
            output::render_error(&format!(
                "Auto-merge failed: {error}. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
        }
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

fn orch_phase_idle_timeout() -> Duration {
    let secs = Config::global()
        .get_param::<u64>(PHASE_IDLE_TIMEOUT_KEY)
        .ok()
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_PHASE_IDLE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Write an orchestration artifact under <artifact_dir>/.goose-orch/<run_id>/.
fn persist_artifact(artifact_dir: &Path, run_id: &str, name: &str, content: &str) {
    let dir = artifact_dir.join(".goose-orch").join(run_id);
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
    stream_role_completion_with_idle_timeout(
        provider,
        model_config,
        system,
        request,
        session_id,
        debug,
        None,
    )
    .await
}

async fn stream_role_completion_with_idle_timeout(
    provider: &Arc<dyn Provider>,
    model_config: &goose_providers::model::ModelConfig,
    system: &str,
    request: Message,
    session_id: &str,
    debug: bool,
    idle_timeout: Option<Duration>,
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
    let _thinking_turn = output::begin_thinking_turn();
    let mut status_tick = tokio::time::interval(output::thinking_status_refresh_interval());
    status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let idle_sleep = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(1)));
    tokio::pin!(idle_sleep);
    let mut timeout_error: Option<String> = None;

    loop {
        tokio::select! {
            next = stream.next() => {
                let Some(next) = next else {
                    break;
                };
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
                if let Some(timeout) = idle_timeout {
                    idle_sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
                }
            }
            _ = status_tick.tick() => {
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
                break;
            }
        }
    }
    output::flush_markdown_buffer_current_theme(&mut buffer);
    output::reset_response_bullet();
    if let Some(error) = timeout_error {
        anyhow::bail!(error);
    }
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
        merge: bool,
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
        let auto_merge = merge
            || config
                .get_param::<bool>("GOOSE_ORCH_AUTO_MERGE")
                .unwrap_or(false);
        let original_working_dir = self.get_session().await?.working_dir;

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
                auto_merge,
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
        if let Err(e) = self
            .agent
            .config
            .session_manager
            .update(&self.session_id)
            .working_dir(original_working_dir)
            .apply()
            .await
        {
            output::render_error(&format!("Failed to restore working directory: {}", e));
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
        auto_merge: bool,
        interactive: bool,
    ) -> Result<OrchOutcome> {
        let config = Config::global();
        let original_dir = std::env::current_dir().context("failed to read current directory")?;
        let run_id = format!("{:x}", ledger::now_ms());
        let workspace = setup_orch_workspace(&original_dir, &run_id);
        render_workspace_banner(&workspace, auto_merge);
        if workspace.is_worktree() {
            self.agent
                .config
                .session_manager
                .update(&self.session_id)
                .working_dir(workspace.impl_dir.clone())
                .apply()
                .await?;
        }
        let working_dir = workspace.impl_dir.display().to_string();

        // Planner and reviewer can be full agents (ACP providers). Plan-Explore
        // gives them full read-only exploration (file reads, search, subagent
        // delegation) while goose's kind-based permission policy rejects every
        // mutating tool call — provider-agnostic, no reliance on the agent's
        // own mode semantics.
        config.set_param("GOOSE_ACP_PLAN_EXPLORE", true)?;

        let session_id = self.session_id.clone();
        let meta = PhaseMeta {
            session_id: &session_id,
            run_id: &run_id,
            task,
        };
        let role_idle_timeout = if interactive {
            None
        } else {
            Some(orch_phase_idle_timeout())
        };

        output::set_active_role_status(Some(output::ActiveRoleStatus {
            role: output::ActiveRole::Planner,
            cycle: None,
        }));
        phase_banner(
            &format!(
                "phase: plan · {}/{}",
                planner_role.provider_name, planner_role.model
            ),
            output::ActiveRole::Planner,
        );
        output::set_thinking_context(Some(format!(
            "{}/{} working…",
            planner_role.provider_name, planner_role.model
        )));
        let phase_started = Instant::now();
        let (planner, planner_model) =
            build_role_provider(planner_role, &workspace.impl_dir).await?;
        output::show_thinking();
        // ACP providers wrap full agent CLIs that may not receive our system prompt,
        // so the role instructions are embedded in the user message as well.
        let plan_request = Message::user().with_text(format!(
            "{}\n\n---\n\nTask:\n{}\n\nWorking directory: {}",
            PLAN_SYSTEM_PROMPT, task, working_dir
        ));
        let planner_system = role_system_prompt(PLAN_SYSTEM_PROMPT, planner_role);
        let (plan_text, plan_usage) = if let Some(timeout) = role_idle_timeout {
            stream_role_completion_with_idle_timeout(
                &planner,
                &planner_model,
                &planner_system,
                plan_request,
                &self.session_id,
                self.debug,
                Some(timeout),
            )
            .await?
        } else {
            stream_role_completion(
                &planner,
                &planner_model,
                &planner_system,
                plan_request,
                &self.session_id,
                self.debug,
            )
            .await?
        };
        output::hide_thinking();
        if plan_text.trim().is_empty() {
            anyhow::bail!("planner produced an empty plan");
        }
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

        persist_artifact(&workspace.original_dir, &run_id, "plan.md", &plan_text);
        println!(
            "  {}",
            console::style(format!("artifacts → .goose-orch/{}/", run_id)).dim()
        );

        let (reviewer, reviewer_model) = if reviewer_role == planner_role {
            (Arc::clone(&planner), planner_model.clone())
        } else {
            build_role_provider(reviewer_role, &workspace.impl_dir).await?
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
            "You are the implementer in a plan/implement/review workflow. Execute the plan below for the task. Modify files and run verification with your tools. When done, report what you changed and how you verified it.{}\n\nTask:\n{}\n\nWorking directory:\n{}\n\nPlan:\n{}",
            implementer_playbook, task, working_dir, plan_text
        );

        for cycle in 1..=max_cycles {
            output::set_active_role_status(Some(output::ActiveRoleStatus {
                role: output::ActiveRole::Implementer,
                cycle: Some((cycle, max_cycles)),
            }));
            phase_banner(
                &format!(
                    "phase: implement (cycle {}/{}) · {}/{}",
                    cycle, max_cycles, implementer_role.provider_name, implementer_role.model
                ),
                output::ActiveRole::Implementer,
            );
            output::set_thinking_context(Some(format!(
                "{}/{} working…",
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

            output::set_active_role_status(Some(output::ActiveRoleStatus {
                role: output::ActiveRole::Reviewer,
                cycle: Some((cycle, max_cycles)),
            }));
            phase_banner(
                &format!(
                    "phase: review (cycle {}/{}) · {}/{}",
                    cycle, max_cycles, reviewer_role.provider_name, reviewer_role.model
                ),
                output::ActiveRole::Reviewer,
            );
            output::set_thinking_context(Some(format!(
                "{}/{} working…",
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
                &workspace.original_dir,
                &run_id,
                &format!("report-c{}.md", cycle),
                &implementer_report,
            );
            if implementer_report.len() > EVIDENCE_CHAR_LIMIT {
                warn_truncated("implementer report", implementer_report.len(), &run_id);
            }
            let evidence = git_evidence(&workspace.impl_dir);
            persist_artifact(
                &workspace.original_dir,
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
            let (review_text, review_usage) = if let Some(timeout) = role_idle_timeout {
                stream_role_completion_with_idle_timeout(
                    &reviewer,
                    &reviewer_model,
                    &reviewer_system,
                    review_request,
                    &self.session_id,
                    self.debug,
                    Some(timeout),
                )
                .await?
            } else {
                stream_role_completion(
                    &reviewer,
                    &reviewer_model,
                    &reviewer_system,
                    review_request,
                    &self.session_id,
                    self.debug,
                )
                .await?
            };
            output::hide_thinking();
            persist_artifact(
                &workspace.original_dir,
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
                finalize_worktree_approval(&workspace, task, auto_merge);
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    fn git(dir: &Path, args: &[&str]) {
        crate::worktree::git(dir, args).expect("git command");
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), &["init"]);
        git(temp.path(), &["config", "user.name", "Goose Test"]);
        git(temp.path(), &["config", "user.email", "goose@example.com"]);
        fs::write(temp.path().join(".gitignore"), ".env\n.goose/\n").expect("write gitignore");
        fs::write(temp.path().join("README.md"), "hello\n").expect("write readme");
        fs::write(temp.path().join(".env"), "ROOT=1\n").expect("write env");
        git(temp.path(), &["add", ".gitignore", "README.md"]);
        git(temp.path(), &["commit", "-m", "initial"]);
        temp
    }

    fn subject(task: &str, paths: &[&str]) -> String {
        super::conventional_commit_subject(
            task,
            &paths
                .iter()
                .map(|path| path.to_string())
                .collect::<Vec<_>>(),
        )
    }

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
        ) -> Result<goose::providers::base::MessageStream, goose_providers::errors::ProviderError>
        {
            use futures::StreamExt;

            let first_text = self.first_text;
            let pending = futures::stream::pending();
            if let Some(first_text) = first_text {
                let first = futures::stream::once(async move {
                    Ok((
                        Some(
                            goose::conversation::message::Message::assistant()
                                .with_text(first_text),
                        ),
                        None,
                    ))
                });
                Ok(Box::pin(first.chain(pending)))
            } else {
                Ok(Box::pin(pending))
            }
        }
    }

    #[tokio::test]
    async fn stream_role_completion_returns_partial_text_after_idle_timeout() {
        let provider: Arc<dyn goose::providers::base::Provider> = Arc::new(SilentProvider {
            first_text: Some("partial plan"),
        });

        let (text, usage) = super::stream_role_completion_with_idle_timeout(
            &provider,
            &goose_providers::model::ModelConfig::new("test-model"),
            "",
            goose::conversation::message::Message::user().with_text("plan this"),
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

        let err = super::stream_role_completion_with_idle_timeout(
            &provider,
            &goose_providers::model::ModelConfig::new("test-model"),
            "",
            goose::conversation::message::Message::user().with_text("plan this"),
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

    #[test]
    fn setup_orch_workspace_creates_named_worktree_branch_and_env_link() {
        let repo = init_repo();
        let repo_root = crate::worktree::find_repo_root(repo.path()).expect("repo root");

        let workspace = super::setup_orch_workspace_with_force(repo.path(), "abc123", false);

        assert!(workspace.is_worktree());
        assert_eq!(
            workspace.impl_dir,
            repo_root.join(".goose/worktrees/orch-abc123")
        );
        assert_eq!(workspace.branch.as_deref(), Some("orch/abc123"));
        assert_eq!(workspace.env_links.len(), 1);
        assert_eq!(
            crate::worktree::current_branch(&workspace.impl_dir).expect("branch"),
            "orch/abc123"
        );
    }

    #[test]
    fn setup_orch_workspace_falls_back_in_place_when_forced_or_outside_git() {
        let repo = init_repo();
        let forced = super::setup_orch_workspace_with_force(repo.path(), "forced", true);
        assert!(!forced.is_worktree());
        assert_eq!(forced.impl_dir, repo.path());
        assert_eq!(
            forced.in_place_reason.as_deref(),
            Some("GOOSE_ORCH_IN_PLACE=true")
        );

        let temp = tempfile::tempdir().expect("tempdir");
        let non_git = super::setup_orch_workspace_with_force(temp.path(), "nongit", false);
        assert!(!non_git.is_worktree());
        assert_eq!(non_git.impl_dir, temp.path());
        assert_eq!(
            non_git.in_place_reason.as_deref(),
            Some("not a git repository")
        );
    }

    #[test]
    fn conventional_commit_subject_defaults_to_feat() {
        assert_eq!(
            subject("Add automatic orch worktrees", &["README.md"]),
            "feat(README.md): add automatic orch worktrees"
        );
    }

    #[test]
    fn conventional_commit_subject_infers_fix_type() {
        assert_eq!(
            subject(
                "Fix orch approval handling.",
                &["crates/goose-cli/src/session/orchestrate.rs"]
            ),
            "fix(cli): fix orch approval handling"
        );
    }

    #[test]
    fn conventional_commit_subject_omits_scope_for_mixed_changes() {
        assert_eq!(
            subject(
                "Refactor provider wiring",
                &[
                    "crates/goose-cli/src/session/orchestrate.rs",
                    "crates/goose/src/providers/base.rs"
                ]
            ),
            "refactor: refactor provider wiring"
        );
    }

    #[test]
    fn conventional_commit_subject_trims_blank_lines_and_long_summary() {
        assert_eq!(
            subject(
                "\n\nAdd a very long orchestration lifecycle summary that should be truncated before it turns into an unwieldy commit subject.",
                &["ui/desktop/src/main.ts"]
            ),
            "feat(ui): add a very long orchestration lifecycle summary that should be..."
        );
    }

    #[test]
    fn parse_status_path_handles_unicode_and_renames() {
        assert_eq!(
            super::parse_status_path("?? 문서.txt"),
            Some("문서.txt".to_string())
        );
        assert_eq!(
            super::parse_status_path("R  old.txt -> 새.txt"),
            Some("새.txt".to_string())
        );
    }
}
