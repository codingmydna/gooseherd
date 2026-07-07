use anyhow::Result;
use console::style;
use goose::config::Config;
use goose::conversation::message::Message;
use goose::utils::safe_truncate;
use std::path::PathBuf;
use std::time::Instant;

use crate::worktree;

use super::orchestrate::{build_role_provider, resolve_all_roles, RoleConfig};
use super::{output, CliSession};

const ARENA_DIR: &str = ".goose-arena";
const LINEUP_KEY: &str = "GOOSE_ARENA_LINEUP";
const TIMEOUT_KEY: &str = "GOOSE_ARENA_TIMEOUT_SECS";
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DIFF_CHAR_LIMIT: usize = 20_000;

const JUDGE_PROMPT: &str = r#"You are judging an arena: several models implemented the same task independently. For each contestant you receive the diff their attempt produced (against the same starting commit) and their runtime. You do not know which model produced which attempt beyond the labels given.

Judge on: correctness for the task, completeness, code quality, and appropriate scope (no unrequested changes). Runtime only breaks ties.

Reply with:
1. A ranking line: `RANKING: <label> > <label> > ...`
2. For each contestant, 2-4 sentences: what they did well and any defects.
Be specific and cite evidence from the diffs."#;

struct Contestant {
    label: String,
    provider: String,
    model: String,
    worktree: PathBuf,
    log_path: PathBuf,
    duration_secs: f64,
    exit_ok: bool,
    diff: String,
    diff_stat: String,
}

fn parse_lineup(spec: &str) -> Vec<(String, String)> {
    spec.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            entry
                .split_once('/')
                .map(|(p, m)| (p.trim().to_string(), m.trim().to_string()))
        })
        .collect()
}

fn log_stdio(path: &std::path::Path) -> (std::process::Stdio, std::process::Stdio) {
    std::fs::File::create(path)
        .and_then(|file| file.try_clone().map(|clone| (file.into(), clone.into())))
        .unwrap_or_else(|_| (std::process::Stdio::null(), std::process::Stdio::null()))
}

impl CliSession {
    pub(super) async fn handle_arena(&mut self, args: String) -> Result<()> {
        let args = args.trim().to_string();
        if args.is_empty() {
            output::render_error(
                "Usage: /arena [lineup=provider/model,provider/model,...] <task> — run the same task on each contestant in an isolated git worktree, then have the reviewer judge the results.",
            );
            return Ok(());
        }

        let config = Config::global();
        let repo_root =
            match worktree::git(&std::env::current_dir()?, &["rev-parse", "--show-toplevel"]) {
                Ok(root) => PathBuf::from(root.trim()),
                Err(_) => {
                    output::render_error("/arena requires a git repository (worktree isolation).");
                    return Ok(());
                }
            };

        let (lineup_spec, task) = if let Some(rest) = args.strip_prefix("lineup=") {
            let Some((lineup, task)) = rest.split_once(' ') else {
                output::render_error("Missing task after lineup=...");
                return Ok(());
            };
            (lineup.to_string(), task.trim().to_string())
        } else {
            let spec = config
                .get_param::<String>(LINEUP_KEY)
                .unwrap_or_else(|_| "codex-acp/gpt-5.5".to_string());
            (spec, args)
        };

        let lineup = parse_lineup(&lineup_spec);
        if lineup.is_empty() {
            output::render_error("Empty lineup. Use lineup=provider/model[,provider/model...]");
            return Ok(());
        }
        let timeout_secs = config
            .get_param::<u64>(TIMEOUT_KEY)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        println!(
            "{}",
            style(format!(
                "arena: {} contestants · timeout {}s · task: {}",
                lineup.len(),
                timeout_secs,
                safe_truncate(&task, 80)
            ))
            .dim()
        );

        let goose_bin = std::env::current_exe()?;
        let base_commit = worktree::git(&repo_root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        let arena_root = repo_root.join(ARENA_DIR);
        std::fs::create_dir_all(&arena_root)?;

        // Launch every contestant concurrently, each in its own worktree.
        let mut handles = Vec::new();
        for (idx, (provider, model)) in lineup.iter().enumerate() {
            let label = format!("{}-{}", (b'A' + idx as u8) as char, provider);
            let worktree = arena_root.join(format!(
                "{}-{}",
                label.to_lowercase(),
                model.replace('/', "_")
            ));
            let _ = worktree::remove_worktree(&repo_root, &worktree, true);
            worktree::create_detached_worktree(&repo_root, &worktree, &base_commit)?;

            println!(
                "  {} {} → {}",
                style("▸").dim(),
                style(format!("{} ({}/{})", label, provider, model)).bold(),
                style(worktree.display().to_string()).dim()
            );

            let prompt = format!(
                "Implement the following task in the current directory. Modify files and verify your work with the available tools. Task:\n{}",
                task
            );
            let goose_bin = goose_bin.clone();
            let provider = provider.clone();
            let model = model.clone();
            let worktree_c = worktree.clone();
            let label_c = label.clone();
            let log_path = arena_root.join(format!("{}.log", label));
            let handle = tokio::spawn(async move {
                let started = Instant::now();
                let (stdout_log, stderr_log) = log_stdio(&log_path);
                let child = tokio::process::Command::new(&goose_bin)
                    .args(["run", "--no-session", "-t", &prompt])
                    .current_dir(&worktree_c)
                    .env("GOOSE_PROVIDER", &provider)
                    .env("GOOSE_MODEL", &model)
                    .env("GOOSE_MODE", "auto")
                    .env("GOOSE_ACP_PLAN_EXPLORE", "false")
                    .stdout(stdout_log)
                    .stderr(stderr_log)
                    .spawn();
                let exit_ok = match child {
                    Ok(mut child) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(timeout_secs),
                            child.wait(),
                        )
                        .await
                        {
                            Ok(Ok(status)) => status.success(),
                            Ok(Err(_)) => false,
                            Err(_) => {
                                let _ = child.kill().await;
                                false
                            }
                        }
                    }
                    Err(_) => false,
                };
                (
                    label_c,
                    provider,
                    model,
                    worktree_c,
                    log_path,
                    started.elapsed(),
                    exit_ok,
                )
            });
            handles.push(handle);
        }

        output::set_thinking_context(Some(format!(
            "arena: {} contestants working in parallel…",
            handles.len()
        )));
        output::show_thinking();
        let mut contestants = Vec::new();
        for handle in handles {
            let (label, provider, model, worktree, log_path, elapsed, exit_ok) = handle.await?;
            let diff = worktree::git(&worktree, &["diff", "HEAD"]).unwrap_or_default();
            let untracked =
                worktree::git(&worktree, &["ls-files", "--others", "--exclude-standard"])
                    .unwrap_or_default();
            let mut full_diff = diff;
            for file in untracked.lines() {
                if let Ok(content) = std::fs::read_to_string(worktree.join(file)) {
                    full_diff.push_str(&format!("\n+++ new file: {}\n{}", file, content));
                }
            }
            let diff_stat =
                worktree::git(&worktree, &["diff", "--stat", "HEAD"]).unwrap_or_default();
            contestants.push(Contestant {
                label,
                provider,
                model,
                worktree,
                log_path,
                duration_secs: elapsed.as_secs_f64(),
                exit_ok,
                diff: safe_truncate(&full_diff, DIFF_CHAR_LIMIT),
                diff_stat,
            });
        }
        output::hide_thinking();

        println!();
        println!("{}", style("arena results").cyan().bold());
        for c in &contestants {
            let status = if !c.exit_ok {
                style("failed/timeout").red().to_string()
            } else if c.diff.trim().is_empty() {
                style("no changes").yellow().to_string()
            } else {
                style("completed").green().to_string()
            };
            println!(
                "  {:<16} {}/{}  {}  {:.0}s  {}",
                style(&c.label).bold(),
                c.provider,
                c.model,
                status,
                c.duration_secs,
                style(c.diff_stat.lines().last().unwrap_or("").trim()).dim()
            );
            println!(
                "  {:<16} {}",
                "",
                style(format!("↳ {}", c.worktree.display())).dim()
            );
            if !c.exit_ok {
                println!(
                    "  {:<16} {}",
                    "",
                    style(format!("↳ log: {}", c.log_path.display())).dim()
                );
            }
        }

        // Blind judging by the reviewer role.
        let roles = resolve_all_roles()?;
        let judge_role: RoleConfig = roles.reviewer;
        println!(
            "\n{}",
            style(format!(
                "judging by {}/{}…",
                judge_role.provider_name, judge_role.model
            ))
            .dim()
        );
        let mut judge_input = format!("Task given to all contestants:\n{}\n", task);
        for c in &contestants {
            judge_input.push_str(&format!(
                "\n=== Contestant {} (runtime {:.0}s{}) ===\n{}\n",
                c.label,
                c.duration_secs,
                if c.exit_ok { "" } else { ", DID NOT FINISH" },
                if c.diff.trim().is_empty() {
                    "(no changes produced)"
                } else {
                    &c.diff
                }
            ));
        }

        config.set_param("GOOSE_ACP_PLAN_EXPLORE", true)?;
        let judge_built = build_role_provider(&judge_role).await;
        config.set_param("GOOSE_ACP_PLAN_EXPLORE", false)?;
        let (judge, judge_model) = judge_built?;

        output::set_thinking_context(Some(format!(
            "judge {}/{} working…",
            judge_role.provider_name, judge_role.model
        )));
        output::show_thinking();
        let (verdict_message, _usage) =
            goose::session_context::with_session_id(
                Some(self.session_id.clone()),
                judge.complete(
                    &judge_model,
                    JUDGE_PROMPT,
                    &[Message::user()
                        .with_text(format!("{}\n\n---\n\n{}", JUDGE_PROMPT, judge_input))],
                    &[],
                ),
            )
            .await?;
        output::hide_thinking();
        output::render_message(&verdict_message, self.debug);

        println!(
            "\n  {}",
            style(format!(
                "worktrees kept for inspection under {} — remove with: git worktree remove --force <dir>",
                arena_root.display()
            ))
            .dim()
        );
        Ok(())
    }
}
