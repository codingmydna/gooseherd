use anyhow::{Context, Result};
use goose::config::{Config, GooseMode};
use goose::conversation::message::Message;
use goose::providers::base::ProviderUsage;
use goose::utils::middle_out_truncate;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::session::{exemplars, ledger, output, plan_exemplars, review_exemplars, CliSession};

use super::gates::{
    derived_gates_notice, gate_banner_line, gate_env_mode, gate_outputs_review_section,
    gate_passed_review_note, next_gate_step, partition_gates, record_gate_phase, resolve_gates,
    run_gates, seed_allowed_commands, GateEnvMode, GateOutcome, GateRun, GateStep,
};
use super::limits::handle_phase_error;
use super::phases::{
    archive_pending_reviews, gate_banner, has_self_verification, orch_phase_idle_timeout,
    orch_progress_cadence, partial_completion_text, persist_artifact, phase_banner, record_phase,
    record_self_verification, self_verification_demand, self_verification_reprompt,
    self_verification_review_block, stream_role_completion, stream_role_completion_status,
    warn_truncated, PendingReviewArchive, PhaseMeta, PhasePolicySummary, EVIDENCE_CHAR_LIMIT,
    REVIEW_SYSTEM_PROMPT,
};
use super::planner::run_plan_phase;
use super::repo_pack;
use super::roles::{
    build_role_provider, implement_policy_label, is_acp_provider, playbook_banner_fragment,
    playbook_text, render_uplift_skip_notice, resolve_all_roles, role_stream_system_prompt,
    user_instruction_preamble, RoleConfig,
};
use super::workspace::{
    finalize_worktree_approval, git_diff_stat, git_evidence, render_workspace_banner,
    setup_orch_workspace,
};
use super::{
    resolve_orch_implement_policy, OrchImplementPolicy, OrchOutcome, DEFAULT_MAX_CYCLES,
    DEFAULT_MAX_GATE_RETRIES, GATES_KEY, MAX_CYCLES_KEY, MAX_GATE_RETRIES_KEY,
};
use crate::session::verdict::{self, ReviewVerdict};

/// Whether a self-verification reprompt reply adds new content to the report.
/// A reply identical to the existing report means the reprompt turn produced no
/// new assistant text (only tool calls, or an interrupted turn), so appending it
/// would duplicate the whole report in the review evidence.
fn sv_reply_is_new(report: &str, reply: &str) -> bool {
    let reply = reply.trim();
    !reply.is_empty() && reply != report.trim()
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
        for key in config.heal_stale_orch_flags() {
            println!(
                "  {}",
                console::style(format!(
                    "cleared stale {key} left in config.yaml by an interrupted older run"
                ))
                .dim()
            );
        }
        let roles = resolve_all_roles()?;
        let default_role = roles.default;
        let planner_role = roles.planner;
        let reviewer_role = roles.reviewer;
        let implementer_role = roles.implementer;
        let implement_policy = resolve_orch_implement_policy(interactive)?;
        let implementer_is_acp = is_acp_provider(&implementer_role.provider_name);
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
        // The orchestration path is authoritative over the implementer's
        // permission mode; save the session's mode so every exit path restores
        // it (the runner installs a GOOSE_MODE override and drives the agent's
        // mode directly, so recreate cannot re-apply a stale cached mode).
        let previous_agent_mode = self.agent.goose_mode().await;

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
        println!(
            "  {}",
            console::style(format!(
                "implement policy: {}",
                implement_policy_label(implement_policy, implementer_is_acp)
            ))
            .dim()
        );

        let outcome = self
            .run_orchestration(
                &task,
                &planner_role,
                &reviewer_role,
                &implementer_role,
                max_cycles,
                auto_merge,
                interactive,
                implement_policy,
            )
            .await;

        output::set_active_role(None);
        output::end_phase_progress();
        output::set_thinking_context(None);
        // Drop the in-memory permission overrides installed for this run. There
        // is no persisted state to undo (see set_runtime_override), so a crash
        // above simply leaves nothing behind. The per-instance denial counter
        // dies with the provider instance.
        config.clear_runtime_override("GOOSE_ACP_PLAN_EXPLORE");
        config.clear_runtime_override(goose::acp::ORCH_IMPLEMENT_ACTIVE_KEY);
        config.clear_runtime_override(goose::acp::ORCH_IMPLEMENT_POLICY_KEY);
        config.clear_runtime_override(goose::acp::ORCH_ALLOWED_COMMANDS_KEY);
        config.clear_runtime_override("GOOSE_MODE");
        // Restore the session's pre-orchestration permission mode on the agent
        // and the (about-to-be-recreated) provider, undoing the orch override.
        if let Err(e) = self
            .agent
            .update_goose_mode(previous_agent_mode, &self.session_id)
            .await
        {
            output::render_error(&format!("Failed to restore session permission mode: {}", e));
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
        implement_policy: OrchImplementPolicy,
    ) -> Result<OrchOutcome> {
        let config = Config::global();
        let original_dir = std::env::current_dir().context("failed to read current directory")?;
        let run_id = format!("{:x}", ledger::now_ms());
        let workspace = setup_orch_workspace(&original_dir, &run_id);
        render_workspace_banner(&workspace, auto_merge);
        let resolved_gates = resolve_gates(
            &workspace.impl_dir,
            Some(&workspace.original_dir),
            config
                .get_param::<Vec<String>>(GATES_KEY)
                .unwrap_or_default(),
        );
        println!(
            "  {}",
            console::style(gate_banner_line(&resolved_gates)).dim()
        );
        if let Some(notice) = derived_gates_notice(&resolved_gates) {
            println!("  {}", console::style(notice).dim());
        }
        if let Some(warning) = &resolved_gates.warning {
            println!(
                "  {} {}",
                console::style("⚠").yellow(),
                console::style(warning).yellow()
            );
        }
        let implementer_is_acp = is_acp_provider(&implementer_role.provider_name);
        println!(
            "  {}",
            console::style(format!(
                "security: implement policy={} · gate env={} · .env linked={}",
                implement_policy_label(implement_policy, implementer_is_acp),
                match gate_env_mode() {
                    GateEnvMode::Scrub => "scrub",
                    GateEnvMode::Inherit => "inherit",
                },
                if workspace.env_linked() { "yes" } else { "no" }
            ))
            .dim()
        );
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

        let repo_pack_root = workspace
            .repo_root
            .clone()
            .unwrap_or_else(|| workspace.original_dir.clone());
        // Stable per-repo key so exemplars archived here are preferred by future
        // runs in the same repo (see exemplar store repo scoping).
        let repo_scope = exemplars::repo_scope_key(&repo_pack_root);
        let repo_pack = if repo_pack::repo_pack_injects(planner_role)
            || repo_pack::repo_pack_injects(implementer_role)
        {
            repo_pack::cached_repo_pack(&repo_pack_root)
        } else {
            None
        };
        let planner_repo_pack = repo_pack
            .as_deref()
            .filter(|_| repo_pack::repo_pack_injects(planner_role));

        config.set_runtime_override("GOOSE_ACP_PLAN_EXPLORE", true)?;

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
        let plan = match run_plan_phase(
            &self.session_id,
            self.debug,
            task,
            &working_dir,
            &workspace.impl_dir,
            &workspace.original_dir,
            &run_id,
            interactive,
            planner_role,
            planner_repo_pack,
            Some(&repo_scope),
            &meta,
        )
        .await
        {
            Ok(plan) => plan,
            Err(err) => {
                return handle_phase_error(
                    err,
                    "planner",
                    planner_role,
                    &run_id,
                    task,
                    reviewer_role,
                    &[],
                    Some(&repo_scope),
                );
            }
        };
        let plan_text = plan.plan_text;
        let planner = plan.planner;
        let planner_model = plan.planner_model;
        let planner_context_limit = plan.planner_context_limit;

        let (reviewer, reviewer_model) = if reviewer_role == planner_role {
            (Arc::clone(&planner), planner_model.clone())
        } else {
            match build_role_provider(reviewer_role, &workspace.impl_dir).await {
                Ok(reviewer) => reviewer,
                Err(err) => {
                    return handle_phase_error(
                        err,
                        "reviewer",
                        reviewer_role,
                        &run_id,
                        task,
                        reviewer_role,
                        &[],
                        Some(&repo_scope),
                    );
                }
            }
        };
        config.clear_runtime_override("GOOSE_ACP_PLAN_EXPLORE");

        let acp_allowlist =
            implementer_is_acp && implement_policy == OrchImplementPolicy::Allowlist;
        let implementer_goose_mode = if acp_allowlist {
            GooseMode::Approve
        } else {
            GooseMode::Auto
        };

        // Steer the implementer's permission mode/policy in-memory only, so a
        // crash mid-run cannot leave the user's on-disk mode downgraded. The
        // implement-policy override is what the provider's ImplementPolicy reads;
        // without it a headless allowlist run silently degrades to `auto`.
        config.set_runtime_override(goose::acp::ORCH_IMPLEMENT_ACTIVE_KEY, acp_allowlist)?;
        config.set_runtime_override(
            goose::acp::ORCH_IMPLEMENT_POLICY_KEY,
            implement_policy.as_config_str(),
        )?;
        config.set_runtime_override("GOOSE_MODE", implementer_goose_mode)?;
        if acp_allowlist
            && config
                .get_param::<serde_json::Value>(goose::acp::ORCH_ALLOWED_COMMANDS_KEY)
                .is_err()
        {
            let seed = seed_allowed_commands(&workspace.impl_dir, &resolved_gates);
            println!(
                "  {}",
                console::style(format!(
                    "seeded implement allowlist: {} (set {} to override)",
                    seed.join(", "),
                    goose::acp::ORCH_ALLOWED_COMMANDS_KEY
                ))
                .dim()
            );
            config.set_runtime_override(goose::acp::ORCH_ALLOWED_COMMANDS_KEY, seed)?;
        }
        // Drive the agent's mode to the orch value BEFORE recreating the
        // implementer provider. recreate ends by re-applying the agent's cached
        // mode to the fresh provider (and the ACP subprocess); without this it
        // would re-send the stale session mode and clobber the GOOSE_MODE
        // override, defeating allowlist enforcement.
        if let Err(err) = self
            .agent
            .update_goose_mode(implementer_goose_mode, &self.session_id)
            .await
        {
            return handle_phase_error(
                err,
                "implementer",
                implementer_role,
                &run_id,
                task,
                reviewer_role,
                &[],
                Some(&repo_scope),
            );
        }
        let impl_model_config = goose::model_config::model_config_from_user_config(
            &implementer_role.provider_name,
            implementer_role.model.as_str(),
        )?;
        if let Err(err) = self
            .agent
            .recreate_provider_for_session(
                &self.session_id,
                &implementer_role.provider_name,
                impl_model_config,
            )
            .await
        {
            return handle_phase_error(
                err,
                "implementer",
                implementer_role,
                &run_id,
                task,
                reviewer_role,
                &[],
                Some(&repo_scope),
            );
        }
        // The freshly built implementer provider captured its policy at
        // construction; the denial counter lives on that instance, so read it
        // back from the session provider rather than any process global.
        let implementer_provider = self.agent.provider().await.ok();

        let implementer_playbook = if implementer_is_acp {
            String::new()
        } else {
            format!("\n\n# Operating playbook\n\n{}", playbook_text())
        };
        let mut instruction = format!(
            "You are the implementer in a plan/implement/review workflow. Execute the plan below for the task. Modify files and run verification with your tools. When done, report what you changed and how you verified it.{}\n\nTask:\n{}\n\nWorking directory:\n{}\n\nPlan:\n{}",
            implementer_playbook, task, working_dir, plan_text
        );
        if let Some(pack) = repo_pack
            .as_deref()
            .filter(|_| repo_pack::repo_pack_injects(implementer_role))
        {
            instruction.push_str(&repo_pack::orientation_block(pack));
            println!(
                "  {}",
                console::style("repo pack injected for implementer").dim()
            );
        }
        let failure_modes = review_exemplars::build_failure_modes_injection(
            task,
            &implementer_role.provider_name,
            &implementer_role.model,
            Some(&repo_scope),
            Some(&run_id),
        );
        if let Some(section) = &failure_modes.prompt_section {
            instruction.push_str("\n\n");
            instruction.push_str(section);
            println!(
                "  {}",
                console::style(format!(
                    "known failure modes injected [{}]",
                    failure_modes.selected_run_ids.join(", ")
                ))
                .dim()
            );
        }
        instruction.push_str(&self_verification_demand(&plan_text));
        render_uplift_skip_notice("implementer", implementer_role);
        let gate_partition = partition_gates(&workspace.impl_dir, &resolved_gates.gates);
        for skip in &gate_partition.skipped {
            println!(
                "  {} {}",
                console::style("⎿").dim(),
                console::style(format!("gate skipped ({}): {}", skip.reason, skip.command)).dim()
            );
        }
        let gates = gate_partition.applicable;
        let max_gate_retries = config
            .get_param::<u32>(MAX_GATE_RETRIES_KEY)
            .ok()
            .unwrap_or(DEFAULT_MAX_GATE_RETRIES);
        let mut gate_retries = 0;
        let gate_note = gate_passed_review_note(&gates);
        let mut pending_review_archives = Vec::new();
        let mut last_gate_runs: Vec<GateRun> = Vec::new();
        render_uplift_skip_notice("reviewer", reviewer_role);

        for cycle in 1..=max_cycles {
            loop {
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
                output::begin_phase_progress(
                    "implement",
                    Some((cycle, max_cycles)),
                    orch_progress_cadence(),
                );
                if let Some(provider) = &implementer_provider {
                    provider.reset_orch_implement_denial_count();
                }
                self.process_agent_response(interactive, CancellationToken::default())
                    .await?;
                output::hide_thinking();
                output::end_phase_progress();
                let policy_summary = PhasePolicySummary {
                    name: implement_policy_label(implement_policy, implementer_is_acp),
                    denials: implementer_provider
                        .as_ref()
                        .map(|provider| provider.orch_implement_denial_count())
                        .unwrap_or(0),
                };
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
                    Some(&policy_summary),
                    None,
                    None,
                    // ACP implementers never receive the playbook (it is only
                    // in the native user instruction), so record real delivery.
                    Some(!implementer_is_acp),
                );

                if gates.is_empty() {
                    break;
                }

                output::set_active_role_status(None);
                gate_banner(&format!(
                    "phase: gate (cycle {}/{}) · {} gate(s)",
                    cycle,
                    max_cycles,
                    gates.len()
                ));
                let gate_started = Instant::now();
                let outcome = run_gates(&workspace.impl_dir, &gates).await;
                let (passed, detail) = match &outcome {
                    GateOutcome::Passed { runs } => {
                        last_gate_runs = runs.clone();
                        (true, String::new())
                    }
                    GateOutcome::Failed { command, .. } => (false, command.clone()),
                };
                record_gate_phase(
                    &meta,
                    cycle,
                    passed,
                    &detail,
                    gate_started.elapsed().as_millis() as u64,
                );

                match next_gate_step(outcome, &mut gate_retries, max_gate_retries) {
                    GateStep::Proceed => break,
                    GateStep::Reimplement(next_instruction) => {
                        instruction = next_instruction;
                    }
                    GateStep::Abort(reason) => {
                        println!(
                            "{}",
                            console::style(format!(
                                "orchestrate: {reason}; aborting without review."
                            ))
                            .red()
                            .bold()
                        );
                        archive_pending_reviews(
                            &pending_review_archives,
                            &run_id,
                            task,
                            reviewer_role,
                            Some(&repo_scope),
                        );
                        return Ok(OrchOutcome::GateFailed);
                    }
                }
            }

            let mut implementer_report = self.last_assistant_text().unwrap_or_default();
            if !has_self_verification(&implementer_report) {
                println!(
                    "  {}",
                    console::style(
                        "implementer report missing ## Self-verification; reprompting once"
                    )
                    .yellow()
                );
                self.push_message(
                    Message::user().with_text(self_verification_reprompt(&plan_text)),
                );
                self.process_agent_response(interactive, CancellationToken::default())
                    .await?;
                let sv_reply = self.last_assistant_text().unwrap_or_default();
                if implementer_report.trim().is_empty() {
                    implementer_report = sv_reply;
                } else if sv_reply_is_new(&implementer_report, &sv_reply) {
                    implementer_report.push_str("\n\n");
                    implementer_report.push_str(&sv_reply);
                }
                let recovered = has_self_verification(&implementer_report);
                if !recovered {
                    println!(
                        "  {}",
                        console::style(
                            "implementer still omitted ## Self-verification after reprompt; proceeding"
                        )
                        .yellow()
                        .bold()
                    );
                }
                record_self_verification(&meta, cycle, implementer_role, recovered);
            }

            output::set_active_role_status(Some(output::ActiveRoleStatus {
                role: output::ActiveRole::Reviewer,
                cycle: Some((cycle, max_cycles)),
            }));
            let review_exemplar_injection = review_exemplars::build_injection(
                task,
                &reviewer_role.provider_name,
                &reviewer_role.model,
                Some(&repo_scope),
                Some(&run_id),
            );
            phase_banner(
                &format!(
                    "phase: review (cycle {}/{}) · {}/{}{}{}",
                    cycle,
                    max_cycles,
                    reviewer_role.provider_name,
                    reviewer_role.model,
                    review_exemplar_injection.banner_fragment_with_label("review exemplars"),
                    playbook_banner_fragment(reviewer_role)
                ),
                output::ActiveRole::Reviewer,
            );
            output::set_thinking_context(Some(format!(
                "{}/{} working…",
                reviewer_role.provider_name, reviewer_role.model
            )));
            let phase_started = Instant::now();
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
            let diff_stat = git_diff_stat(&workspace.impl_dir);
            let gate_outputs = gate_outputs_review_section(&last_gate_runs, 40);
            let self_verification_checklist =
                self_verification_review_block(&plan_text, &implementer_report);
            let mut review_request_text = format!(
                "{}Task:\n{}\n\nPlan:\n{}\n\nGit evidence:\n{}\n\n",
                user_instruction_preamble(REVIEW_SYSTEM_PROMPT, reviewer_role),
                task,
                plan_text,
                evidence.text,
            );
            if !diff_stat.is_empty() {
                review_request_text.push_str(&format!("Diffstat:\n{}\n\n", diff_stat));
            }
            if !gate_outputs.is_empty() {
                review_request_text.push_str(&gate_outputs);
                review_request_text.push_str("\n\n");
            }
            review_request_text.push_str(&format!(
                "Implementer report:\n{}\n\n{}\n\nWorking directory: {}{}",
                middle_out_truncate(&implementer_report, 8_000, 22_000),
                self_verification_checklist,
                working_dir,
                gate_note
            ));
            if let Some(prompt_section) = &review_exemplar_injection.prompt_section {
                review_request_text.push_str("\n\n---\n\n");
                review_request_text.push_str(prompt_section);
            }
            let review_request = Message::user().with_text(review_request_text);
            output::show_thinking();
            output::begin_phase_progress(
                "review",
                Some((cycle, max_cycles)),
                orch_progress_cadence(),
            );
            let reviewer_system = role_stream_system_prompt(REVIEW_SYSTEM_PROMPT, reviewer_role);
            let (review_text, review_usage) = if let Some(timeout) = role_idle_timeout {
                let completion = match stream_role_completion_status(
                    &reviewer,
                    &reviewer_model,
                    &reviewer_system,
                    std::slice::from_ref(&review_request),
                    &self.session_id,
                    self.debug,
                    Some(timeout),
                )
                .await
                {
                    Ok(completion) => completion,
                    Err(err) => {
                        if let Some(partial_text) = partial_completion_text(&err) {
                            persist_artifact(
                                &workspace.original_dir,
                                &run_id,
                                &format!("review-c{}.partial.md", cycle),
                                partial_text,
                            );
                        }
                        output::end_phase_progress();
                        return handle_phase_error(
                            err,
                            "reviewer",
                            reviewer_role,
                            &run_id,
                            task,
                            reviewer_role,
                            &pending_review_archives,
                            Some(&repo_scope),
                        );
                    }
                };
                (completion.text, completion.usage)
            } else {
                match stream_role_completion(
                    &reviewer,
                    &reviewer_model,
                    &reviewer_system,
                    std::slice::from_ref(&review_request),
                    &self.session_id,
                    self.debug,
                )
                .await
                {
                    Ok(completion) => completion,
                    Err(err) => {
                        if let Some(partial_text) = partial_completion_text(&err) {
                            persist_artifact(
                                &workspace.original_dir,
                                &run_id,
                                &format!("review-c{}.partial.md", cycle),
                                partial_text,
                            );
                        }
                        output::end_phase_progress();
                        return handle_phase_error(
                            err,
                            "reviewer",
                            reviewer_role,
                            &run_id,
                            task,
                            reviewer_role,
                            &pending_review_archives,
                            Some(&repo_scope),
                        );
                    }
                }
            };
            output::hide_thinking();
            output::end_phase_progress();
            persist_artifact(
                &workspace.original_dir,
                &run_id,
                &format!("review-c{}.md", cycle),
                &review_text,
            );
            let reviewer_context_limit = reviewer.get_context_limit(&reviewer_model).await.ok();
            let review_verdict = match verdict::parse_review_verdict(&review_text) {
                Some(review_verdict) => review_verdict,
                None => {
                    println!(
                        "  {}",
                        console::style(
                            "reviewer verdict line missing or malformed; reprompting once"
                        )
                        .yellow()
                    );
                    self.reprompt_review_verdict(
                        &reviewer,
                        &reviewer_model,
                        &reviewer_system,
                        &review_request,
                        &review_text,
                        role_idle_timeout,
                    )
                    .await
                }
            };
            let approved = review_verdict.approved();
            let verdict = review_verdict.ledger_str();
            pending_review_archives.push(PendingReviewArchive {
                cycle,
                verdict: verdict.to_string(),
                review_text: review_text.clone(),
                reviewer_context_limit,
                reviewed_at_ms: ledger::now_ms(),
            });
            record_phase(
                &meta,
                "review",
                cycle,
                "reviewer",
                reviewer_role,
                review_usage.as_ref(),
                reviewer_context_limit,
                phase_started.elapsed().as_millis() as u64,
                Some(verdict),
                None,
                None,
                Some(&review_exemplar_injection),
                None,
            );

            if approved {
                archive_pending_reviews(
                    &pending_review_archives,
                    &run_id,
                    task,
                    reviewer_role,
                    Some(&repo_scope),
                );
                plan_exemplars::archive_approved_plan(
                    true,
                    &plan_exemplars::ArchiveRequest {
                        run_id: &run_id,
                        task,
                        plan_text: &plan_text,
                        planner_provider: &planner_role.provider_name,
                        planner_model: &planner_role.model,
                        planner_context_limit,
                        repo_root: Some(&repo_scope),
                        approved_at_ms: ledger::now_ms(),
                    },
                );
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
                archive_pending_reviews(
                    &pending_review_archives,
                    &run_id,
                    task,
                    reviewer_role,
                    Some(&repo_scope),
                );
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
                "The reviewer did not approve the implementation. Address every item in the review feedback below, then re-verify and report.\n\nReview feedback:\n{}{}",
                review_text,
                self_verification_demand(&plan_text)
            );
        }
        Ok(OrchOutcome::MaxCycles)
    }

    /// One bounded reprompt when the reviewer omitted or malformed its verdict
    /// line. Replays the review request and the reviewer's reply, asks for only
    /// the verdict line, and falls back to `NoVerdict` if that also fails.
    /// Routed through the idle-timeout streaming helper so a reviewer that
    /// already stalled once cannot hang a headless run indefinitely.
    async fn reprompt_review_verdict(
        &self,
        reviewer: &Arc<dyn goose::providers::base::Provider>,
        reviewer_model: &goose_providers::model::ModelConfig,
        reviewer_system: &str,
        review_request: &Message,
        review_text: &str,
        idle_timeout: Option<std::time::Duration>,
    ) -> ReviewVerdict {
        let messages = vec![
            review_request.clone(),
            Message::assistant().with_text(review_text),
            Message::user().with_text(verdict::REVIEW_REPROMPT),
        ];
        match stream_role_completion_status(
            reviewer,
            reviewer_model,
            reviewer_system,
            &messages,
            &self.session_id,
            self.debug,
            idle_timeout,
        )
        .await
        {
            Ok(completion) => {
                verdict::parse_review_verdict(&completion.text).unwrap_or(ReviewVerdict::NoVerdict)
            }
            Err(_) => ReviewVerdict::NoVerdict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sv_reply_is_new;

    #[test]
    fn sv_reply_identical_to_report_is_not_appended() {
        let report = "## Summary\nDid the work.\n\n## Self-verification\n- criterion: ok";
        // The reprompt turn added no new assistant text: last_assistant_text
        // returned the original report verbatim → must not duplicate it.
        assert!(!sv_reply_is_new(report, report));
        assert!(!sv_reply_is_new(report, &format!("  {report}  ")));
        assert!(!sv_reply_is_new(report, "   "));
    }

    #[test]
    fn sv_reply_with_new_text_is_appended() {
        let report = "## Summary\nDid the work.";
        assert!(sv_reply_is_new(
            report,
            "## Self-verification\n- criterion: ok"
        ));
    }
}
