use anyhow::Result;
use console::style;
use goose::config::Config;
use goose::conversation::message::Message;
use goose::utils::safe_truncate;
use std::path::PathBuf;
use std::time::Instant;

use crate::worktree;

use super::orchestrate::{
    build_role_provider, resolve_gates, resolve_judge_role, seed_allowed_commands, RoleConfig,
};
use super::{exemplars, ledger, output, plan_exemplars, CliSession};

const ARENA_DIR: &str = ".goose-arena";
const LINEUP_KEY: &str = "GOOSE_ARENA_LINEUP";
const TIMEOUT_KEY: &str = "GOOSE_ARENA_TIMEOUT_SECS";
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const DIFF_CHAR_LIMIT: usize = 20_000;

const JUDGE_PROMPT: &str = r#"You are judging an arena: several models implemented the same task independently. For each contestant you receive the diff their attempt produced (against the same starting commit) and their runtime. Contestants are identified only by an opaque letter — you do not know which model produced which attempt.

Judge on: correctness for the task, completeness, code quality, and appropriate scope (no unrequested changes). Runtime only breaks ties.

Reply with:
1. A ranking line: `RANKING: <label> > <label> > ...`
2. For each contestant, 2-4 sentences: what they did well and any defects.
Be specific and cite evidence from the diffs."#;

struct Contestant {
    label: String,
    provider: String,
    model: String,
    log_path: PathBuf,
    duration_secs: f64,
    exit_ok: bool,
    diff: String,
    diff_stat: String,
}

/// Permission environment a contestant subprocess runs under. Arena is always
/// headless, so it is safe-by-default: an ACP contestant gets the workspace
/// allowlist unless the user explicitly set `GOOSE_ORCH_IMPLEMENT_POLICY=auto`.
/// Native providers have no allowlist enforcement path and a headless approve
/// prompt would hang, so they stay on `auto` inside their isolated worktree.
fn contestant_security_env(
    provider: &str,
    explicit_policy: Option<&str>,
    allowed_commands: &str,
) -> Vec<(&'static str, String)> {
    let is_acp = provider.ends_with("-acp");
    let opted_out = explicit_policy.is_some_and(|policy| policy.eq_ignore_ascii_case("auto"));
    if is_acp && !opted_out {
        vec![
            ("GOOSE_MODE", "approve".to_string()),
            ("GOOSE_ORCH_IMPLEMENT_ACTIVE", "true".to_string()),
            ("GOOSE_ORCH_IMPLEMENT_POLICY", "allowlist".to_string()),
            ("GOOSE_ORCH_ALLOWED_COMMANDS", allowed_commands.to_string()),
        ]
    } else {
        vec![("GOOSE_MODE", "auto".to_string())]
    }
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

/// Deterministic 64-bit FNV-1a hash, used to shuffle the lineup from the run id
/// without pulling in an RNG.
fn stable_hash(input: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A stable run id for one arena, derived from the session and task so the shuffle
/// is reproducible for the same invocation but varies across tasks.
fn arena_run_id(session_id: &str, task: &str) -> String {
    format!(
        "arena-{:016x}",
        stable_hash(&format!("{session_id}\u{0}{task}"))
    )
}

/// Assign bare letters (A, B, …) to the lineup in a deterministic, model-blind
/// order seeded from the run id. Returns positions as `(label, provider, model)`.
fn shuffled_labels(lineup: &[(String, String)], run_id: &str) -> Vec<(String, String, String)> {
    let mut order: Vec<usize> = (0..lineup.len()).collect();
    order.sort_by_key(|&index| {
        stable_hash(&format!(
            "{run_id}:{index}:{}/{}",
            lineup[index].0, lineup[index].1
        ))
    });
    order
        .into_iter()
        .enumerate()
        .map(|(position, index)| {
            let label = ((b'A' + position as u8) as char).to_string();
            (label, lineup[index].0.clone(), lineup[index].1.clone())
        })
        .collect()
}

/// Parse the judge's `RANKING: A > B > ...` line into ordered labels, keeping only
/// known single-letter labels and dropping duplicates.
fn parse_ranking(text: &str, valid: &[String]) -> Vec<String> {
    let Some(line) = text
        .lines()
        .find(|line| line.trim().to_ascii_lowercase().starts_with("ranking:"))
    else {
        return Vec::new();
    };
    let lower = line.to_ascii_lowercase();
    let start = lower.find("ranking:").map(|i| i + "ranking:".len());
    let Some(after) = start.and_then(|i| line.get(i..)) else {
        return Vec::new();
    };

    let mut ranked = Vec::new();
    for ch in after.chars() {
        let label = ch.to_ascii_uppercase().to_string();
        if valid.contains(&label) && !ranked.contains(&label) {
            ranked.push(label);
        }
    }
    ranked
}

impl CliSession {
    pub(super) async fn handle_arena(&mut self, args: String) -> Result<()> {
        let args = args.trim().to_string();
        if args.is_empty() {
            output::render_error(
                "Usage: /arena [lineup=provider/model,provider/model,...] <task> — run the same task on each contestant in an isolated git worktree, then have the judge blind-rank the results.",
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

        let run_id = arena_run_id(&self.session_id, &task);
        let repo_scope = exemplars::repo_scope_key(&repo_root);
        let contestants_lineup = shuffled_labels(&lineup, &run_id);

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

        // Safe-by-default posture for every contestant: seed a workspace command
        // allowlist from the repo and reuse it for each ACP contestant.
        let explicit_policy = config
            .get_param::<String>("GOOSE_ORCH_IMPLEMENT_POLICY")
            .ok();
        let seed = config
            .get_param::<String>("GOOSE_ORCH_ALLOWED_COMMANDS")
            .unwrap_or_else(|_| {
                let resolved = resolve_gates(&repo_root, None, Vec::new());
                seed_allowed_commands(&repo_root, &resolved).join(",")
            });
        let any_acp = lineup
            .iter()
            .any(|(provider, _)| provider.ends_with("-acp"));
        println!(
            "  {}",
            style(format!(
                "security: contestants headless in isolated worktrees · implement policy={}",
                if any_acp
                    && !explicit_policy
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case("auto"))
                {
                    "allowlist (acp) / auto (native)"
                } else {
                    "auto"
                }
            ))
            .dim()
        );

        // Launch every contestant concurrently, each in its own worktree. The
        // worktree is named for the opaque label only, never the model, so the
        // path the judge might see cannot leak the contestant's identity.
        let mut handles = Vec::new();
        for (label, provider, model) in &contestants_lineup {
            let worktree = arena_root.join(label.to_lowercase());
            let _ = worktree::remove_worktree(&repo_root, &worktree, true);
            worktree::create_detached_worktree(&repo_root, &worktree, &base_commit)?;

            println!(
                "  {} {} → {}",
                style("▸").dim(),
                style(format!("contestant {}", label)).bold(),
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
            let security_env =
                contestant_security_env(&provider, explicit_policy.as_deref(), &seed);
            let handle = tokio::spawn(async move {
                let started = Instant::now();
                let (stdout_log, stderr_log) = log_stdio(&log_path);
                let mut command = tokio::process::Command::new(&goose_bin);
                command
                    .args(["run", "--no-session", "-t", &prompt])
                    .current_dir(&worktree_c)
                    .env("GOOSE_PROVIDER", &provider)
                    .env("GOOSE_MODEL", &model)
                    .env("GOOSE_ACP_PLAN_EXPLORE", "false");
                for (key, value) in &security_env {
                    command.env(key, value);
                }
                let child = command.stdout(stdout_log).stderr(stderr_log).spawn();
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
        let _thinking_turn = output::begin_thinking_turn();
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
                log_path,
                duration_secs: elapsed.as_secs_f64(),
                exit_ok,
                diff: safe_truncate(&full_diff, DIFF_CHAR_LIMIT),
                diff_stat,
            });
        }
        output::hide_thinking();
        drop(_thinking_turn);

        // Pre-verdict results stay blind: label, status, runtime, diffstat only —
        // the model behind each letter is revealed after the verdict renders.
        println!();
        println!("{}", style("arena results (blind)").cyan().bold());
        for c in &contestants {
            let status = if !c.exit_ok {
                style("failed/timeout").red().to_string()
            } else if c.diff.trim().is_empty() {
                style("no changes").yellow().to_string()
            } else {
                style("completed").green().to_string()
            };
            println!(
                "  {:<12} {}  {:.0}s  {}",
                style(format!("contestant {}", c.label)).bold(),
                status,
                c.duration_secs,
                style(c.diff_stat.lines().last().unwrap_or("").trim()).dim()
            );
        }

        // Blind judging by the judge role.
        let judge_role = resolve_judge_role()?;
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

        config.set_runtime_override("GOOSE_ACP_PLAN_EXPLORE", true)?;
        let current_dir = std::env::current_dir()?;
        let judge_built = build_role_provider(&judge_role, &current_dir).await;
        config.clear_runtime_override("GOOSE_ACP_PLAN_EXPLORE");
        let (judge, judge_model) = judge_built?;

        output::set_thinking_context(Some(format!(
            "judge {}/{} working…",
            judge_role.provider_name, judge_role.model
        )));
        let _thinking_turn = output::begin_thinking_turn();
        output::show_thinking();
        let judge_started = Instant::now();
        let (verdict_message, judge_usage) =
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
        let judge_elapsed_ms = judge_started.elapsed().as_millis() as u64;
        output::hide_thinking();
        drop(_thinking_turn);
        output::render_message(&verdict_message, self.debug);

        let labels: Vec<String> = contestants.iter().map(|c| c.label.clone()).collect();
        let ranking = parse_ranking(&verdict_message.as_concat_text(), &labels);
        self.record_arena_ledger(&run_id, &task, &contestants, &ranking);
        self.record_arena_judge_ledger(&run_id, &task, &judge_role, &judge_usage, judge_elapsed_ms);
        self.archive_arena_winner(&run_id, &task, &contestants, &ranking, &repo_scope);

        reveal_lineup(&contestants, &ranking);

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

    /// One ledger row per contestant so `/stats` can count arena wins per model.
    fn record_arena_ledger(
        &self,
        run_id: &str,
        task: &str,
        contestants: &[Contestant],
        ranking: &[String],
    ) {
        for c in contestants {
            let rank = ranking
                .iter()
                .position(|label| label == &c.label)
                .map(|position| position as u32 + 1);
            let winner = ranking.first().map(|winner| winner == &c.label);
            ledger::append(&ledger::PhaseRecord {
                ts_ms: ledger::now_ms(),
                session_id: self.session_id.clone(),
                run_id: run_id.to_string(),
                phase: "arena".to_string(),
                cycle: 0,
                role: "contestant".to_string(),
                provider: c.provider.clone(),
                config_model: c.model.clone(),
                reported_model: None,
                context_limit: None,
                input_tokens: None,
                output_tokens: None,
                duration_ms: (c.duration_secs * 1000.0) as u64,
                verdict: None,
                permission_policy: None,
                permission_denials: None,
                task_preview: safe_truncate(task, 120),
                plan_exemplars_injected: None,
                plan_exemplar_run_ids: None,
                review_exemplars_injected: None,
                review_exemplar_run_ids: None,
                playbook_injected: None,
                arena_rank: rank,
                arena_winner: winner,
            });
        }
    }

    /// The judge's own ledger row, carrying its model identity and token usage.
    fn record_arena_judge_ledger(
        &self,
        run_id: &str,
        task: &str,
        judge_role: &RoleConfig,
        usage: &goose::providers::base::ProviderUsage,
        elapsed_ms: u64,
    ) {
        ledger::append(&ledger::PhaseRecord {
            ts_ms: ledger::now_ms(),
            session_id: self.session_id.clone(),
            run_id: run_id.to_string(),
            phase: "arena-judge".to_string(),
            cycle: 0,
            role: "judge".to_string(),
            provider: judge_role.provider_name.clone(),
            config_model: judge_role.model.clone(),
            reported_model: Some(usage.model.clone()),
            context_limit: None,
            input_tokens: usage.usage.input_tokens.map(|n| n as i64),
            output_tokens: usage.usage.output_tokens.map(|n| n as i64),
            duration_ms: elapsed_ms,
            verdict: None,
            permission_policy: None,
            permission_denials: None,
            task_preview: safe_truncate(task, 120),
            plan_exemplars_injected: None,
            plan_exemplar_run_ids: None,
            review_exemplars_injected: None,
            review_exemplar_run_ids: None,
            playbook_injected: None,
            arena_rank: None,
            arena_winner: None,
        });
    }

    /// Feed the arena's winner back into the hill-climbing loop: archive the
    /// winning approach as a plan exemplar candidate, repo-scoped, so future
    /// planners on similar tasks in this repo can retrieve it.
    fn archive_arena_winner(
        &self,
        run_id: &str,
        task: &str,
        contestants: &[Contestant],
        ranking: &[String],
        repo_scope: &str,
    ) {
        let Some(winner_label) = ranking.first() else {
            return;
        };
        let Some(winner) = contestants.iter().find(|c| &c.label == winner_label) else {
            return;
        };
        if winner.diff.trim().is_empty() {
            return;
        }

        let report = format!(
            "# Arena-winning approach\n\nTask:\n{task}\n\nWinning implementation diff:\n{}\n",
            winner.diff
        );
        let archived = plan_exemplars::archive_approved_plan(
            true,
            &plan_exemplars::ArchiveRequest {
                run_id,
                task,
                plan_text: &report,
                planner_provider: &winner.provider,
                planner_model: &winner.model,
                planner_context_limit: None,
                repo_root: Some(repo_scope),
                approved_at_ms: ledger::now_ms(),
            },
        );
        if archived {
            println!(
                "  {}",
                style("winning approach archived as a plan exemplar candidate").dim()
            );
        }
    }
}

/// Reveal the sealed letter → model map after the verdict, marking the winner.
fn reveal_lineup(contestants: &[Contestant], ranking: &[String]) {
    println!("\n{}", style("lineup (revealed)").cyan().bold());
    for c in contestants {
        let is_winner = ranking.first().is_some_and(|winner| winner == &c.label);
        let rank = ranking
            .iter()
            .position(|label| label == &c.label)
            .map(|position| format!("#{}", position + 1))
            .unwrap_or_else(|| "unranked".to_string());
        let line = format!(
            "{} → {}/{}  ({})",
            c.label,
            c.provider,
            c.model,
            if is_winner {
                style("winner").green().bold().to_string()
            } else {
                style(rank).dim().to_string()
            }
        );
        println!("  {}  ↳ {}", line, style(c.log_path.display()).dim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(
        pairs: &[(&'static str, String)],
    ) -> std::collections::HashMap<&'static str, String> {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn contestant_security_env_defaults_acp_to_allowlist() {
        let env = env_map(&contestant_security_env("codex-acp", None, "git,cargo"));
        assert_eq!(env.get("GOOSE_MODE").map(String::as_str), Some("approve"));
        assert_eq!(
            env.get("GOOSE_ORCH_IMPLEMENT_POLICY").map(String::as_str),
            Some("allowlist")
        );
        assert_eq!(
            env.get("GOOSE_ORCH_IMPLEMENT_ACTIVE").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            env.get("GOOSE_ORCH_ALLOWED_COMMANDS").map(String::as_str),
            Some("git,cargo")
        );
    }

    #[test]
    fn contestant_security_env_keeps_native_on_auto() {
        let env = env_map(&contestant_security_env("openai", None, "git,cargo"));
        assert_eq!(env.get("GOOSE_MODE").map(String::as_str), Some("auto"));
        assert!(!env.contains_key("GOOSE_ORCH_IMPLEMENT_POLICY"));
    }

    #[test]
    fn contestant_security_env_honors_explicit_auto_opt_out() {
        let env = env_map(&contestant_security_env("codex-acp", Some("auto"), "git"));
        assert_eq!(env.get("GOOSE_MODE").map(String::as_str), Some("auto"));
        assert!(!env.contains_key("GOOSE_ORCH_IMPLEMENT_ACTIVE"));
    }

    #[test]
    fn shuffle_is_deterministic_and_assigns_bare_letters() {
        let lineup = vec![
            ("codex-acp".to_string(), "gpt-5.5".to_string()),
            ("claude-acp".to_string(), "opus".to_string()),
            ("openai".to_string(), "gpt-5.5-mini".to_string()),
        ];
        let first = shuffled_labels(&lineup, "arena-abc");
        let second = shuffled_labels(&lineup, "arena-abc");
        assert_eq!(first, second, "same run id must yield the same order");

        let labels: Vec<&str> = first.iter().map(|(label, _, _)| label.as_str()).collect();
        assert_eq!(labels, vec!["A", "B", "C"]);
        // Every original contestant is present exactly once.
        let mut models: Vec<&str> = first.iter().map(|(_, _, model)| model.as_str()).collect();
        models.sort();
        assert_eq!(models, vec!["gpt-5.5", "gpt-5.5-mini", "opus"]);
    }

    #[test]
    fn shuffle_varies_with_run_id() {
        let lineup: Vec<(String, String)> =
            (0..6).map(|i| ("p".to_string(), format!("m{i}"))).collect();
        let a = shuffled_labels(&lineup, "arena-1");
        let b = shuffled_labels(&lineup, "arena-2");
        // The label→model assignment should differ for different run ids.
        assert_ne!(
            a.iter().map(|(_, _, m)| m.clone()).collect::<Vec<_>>(),
            b.iter().map(|(_, _, m)| m.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_ranking_extracts_known_labels_in_order() {
        let labels = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let text = "Some preamble.\nRANKING: B > A > C\nB did the best.";
        assert_eq!(parse_ranking(text, &labels), vec!["B", "A", "C"]);
    }

    #[test]
    fn parse_ranking_is_case_insensitive_and_ignores_unknown_labels() {
        let labels = vec!["A".to_string(), "B".to_string()];
        let text = "ranking: b > z > a";
        assert_eq!(parse_ranking(text, &labels), vec!["B", "A"]);
    }

    #[test]
    fn parse_ranking_absent_returns_empty() {
        let labels = vec!["A".to_string(), "B".to_string()];
        assert!(parse_ranking("no ranking line here", &labels).is_empty());
    }
}
