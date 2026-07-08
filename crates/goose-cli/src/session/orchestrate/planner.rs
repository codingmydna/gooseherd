use anyhow::Result;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::session::{orch_ask, output, plan_exemplars};

use super::phases::{
    orch_ask_enabled, orch_max_question_rounds, orch_min_plan_chars, orch_phase_idle_timeout,
    persist_artifact, phase_banner, plan_quality_action, plan_round_action, planner_prompt,
    record_phase, record_question_round, render_auto_answer_banner, stream_role_completion_status,
    PhaseMeta, PlanQualityAction, PlanRoundAction,
};
use super::roles::{build_role_provider, playbook_banner_fragment, role_system_prompt, RoleConfig};

pub(super) struct PlanPhaseOutput {
    pub(super) plan_text: String,
    pub(super) planner: Arc<dyn Provider>,
    pub(super) planner_model: goose_providers::model::ModelConfig,
    pub(super) planner_context_limit: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_plan_phase(
    session_id: &str,
    debug: bool,
    task: &str,
    working_dir: &str,
    impl_dir: &Path,
    artifact_dir: &Path,
    run_id: &str,
    interactive: bool,
    planner_role: &RoleConfig,
    meta: &PhaseMeta<'_>,
) -> Result<PlanPhaseOutput> {
    let role_idle_timeout = if interactive {
        None
    } else {
        Some(orch_phase_idle_timeout())
    };
    let plan_exemplar_injection =
        plan_exemplars::build_injection(task, &planner_role.provider_name, &planner_role.model);

    output::set_active_role_status(Some(output::ActiveRoleStatus {
        role: output::ActiveRole::Planner,
        cycle: None,
    }));
    phase_banner(
        &format!(
            "phase: plan · {}/{}{}{}",
            planner_role.provider_name,
            planner_role.model,
            plan_exemplar_injection.banner_fragment(),
            playbook_banner_fragment(planner_role)
        ),
        output::ActiveRole::Planner,
    );
    output::set_thinking_context(Some(format!(
        "{}/{} working…",
        planner_role.provider_name, planner_role.model
    )));
    let phase_started = Instant::now();
    let (planner, planner_model) = build_role_provider(planner_role, impl_dir).await?;
    let ask_enabled = orch_ask_enabled();
    let planner_instructions = planner_prompt(ask_enabled);
    let mut plan_request_text = format!(
        "{}\n\n---\n\nTask:\n{}\n\nWorking directory: {}",
        planner_instructions, task, working_dir
    );
    if let Some(prompt_section) = &plan_exemplar_injection.prompt_section {
        plan_request_text.push_str("\n\n---\n\n");
        plan_request_text.push_str(prompt_section);
    }
    let plan_request = Message::user().with_text(plan_request_text);
    let planner_system = role_system_prompt(&planner_instructions, planner_role);
    let max_question_rounds = orch_max_question_rounds();
    let ui_available = interactive && std::io::stdout().is_terminal();
    let mut planner_messages = vec![plan_request];
    let mut qa_rounds: Vec<(orch_ask::OrchQuestionSet, Vec<orch_ask::OrchAnswer>)> = Vec::new();
    let mut question_rounds = 0;
    let mut force_final_plan = false;
    let min_plan_chars = orch_min_plan_chars();
    let mut short_plan_retries = 0;

    let (mut plan_text, plan_usage) = loop {
        output::show_thinking();
        let completion = if let Some(timeout) = role_idle_timeout {
            stream_role_completion_status(
                &planner,
                &planner_model,
                &planner_system,
                &planner_messages,
                session_id,
                debug,
                Some(timeout),
            )
            .await?
        } else {
            stream_role_completion_status(
                &planner,
                &planner_model,
                &planner_system,
                &planner_messages,
                session_id,
                debug,
                None,
            )
            .await?
        };
        output::hide_thinking();
        let planner_text = completion.text;
        if planner_text.trim().is_empty() {
            anyhow::bail!("planner produced an empty plan");
        }

        let parsed = if ask_enabled && !force_final_plan {
            orch_ask::parse_orch_question_block(&planner_text)
        } else {
            None
        };
        match plan_round_action(
            question_rounds,
            max_question_rounds,
            ask_enabled && !force_final_plan,
            parsed.is_some(),
        ) {
            PlanRoundAction::Finalize => {
                if let Some(question_set) = parsed {
                    let answers = orch_ask::auto_recommended_answers(&question_set);
                    render_auto_answer_banner(&question_set, &answers, "round limit");
                    record_question_round(
                        meta,
                        question_rounds + 1,
                        planner_role,
                        &question_set,
                        &answers,
                        "auto-recommended round limit",
                    );
                    planner_messages.push(Message::assistant().with_text(planner_text));
                    let mut reply = orch_ask::format_answers_message(&question_set, &answers);
                    reply.push_str(
                        "\nQuestion round limit reached. Do not ask more questions; produce the plan now.",
                    );
                    planner_messages.push(Message::user().with_text(reply));
                    qa_rounds.push((question_set, answers));
                    force_final_plan = true;
                    continue;
                }
                if completion.idle_timed_out {
                    match plan_quality_action(&planner_text, min_plan_chars, short_plan_retries) {
                        PlanQualityAction::Accept => {}
                        PlanQualityAction::Retry => {
                            short_plan_retries += 1;
                            let actual_chars = planner_text.trim().chars().count();
                            println!(
                                "  {}",
                                console::style(format!(
                                    "planner output after idle timeout was short ({actual_chars}/{min_plan_chars} chars); retrying plan once"
                                ))
                                .yellow()
                                .bold()
                            );
                            planner_messages.push(Message::assistant().with_text(planner_text));
                            planner_messages.push(Message::user().with_text(format!(
                                "Your previous plan was only {actual_chars} characters after the idle timeout, below the minimum of {min_plan_chars}. Produce a complete implementation plan now. Do not ask more questions."
                            )));
                            force_final_plan = true;
                            continue;
                        }
                        PlanQualityAction::Abort => {
                            let actual_chars = planner_text.trim().chars().count();
                            anyhow::bail!(
                                "planner produced an abnormally short plan after retry ({actual_chars}/{min_plan_chars} chars); aborting"
                            );
                        }
                    }
                }
                break (planner_text, completion.usage);
            }
            PlanRoundAction::Ask => {
                let question_set = parsed.expect("question exists for ask action");
                let round = question_rounds + 1;
                question_rounds = round;
                phase_banner(
                    &format!(
                        "phase: plan · question round {}/{}",
                        round, max_question_rounds
                    ),
                    output::ActiveRole::Planner,
                );
                let outcome = if ui_available {
                    match orch_ask::run_ask_ui(&console::Term::stdout(), &question_set) {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            println!(
                                "  {}",
                                console::style(format!(
                                    "question UI failed ({error}); planner will decide"
                                ))
                                .yellow()
                            );
                            orch_ask::AskOutcome::Cancelled
                        }
                    }
                } else {
                    let answers = orch_ask::auto_recommended_answers(&question_set);
                    render_auto_answer_banner(&question_set, &answers, "non-interactive");
                    record_question_round(
                        meta,
                        round,
                        planner_role,
                        &question_set,
                        &answers,
                        "auto-recommended non-interactive",
                    );
                    orch_ask::AskOutcome::Submitted(answers)
                };

                planner_messages.push(Message::assistant().with_text(planner_text));
                match outcome {
                    orch_ask::AskOutcome::Submitted(answers) => {
                        let reply = orch_ask::format_answers_message(&question_set, &answers);
                        planner_messages.push(Message::user().with_text(reply));
                        qa_rounds.push((question_set, answers));
                    }
                    orch_ask::AskOutcome::Chat {
                        question_index,
                        text,
                    } => {
                        let reply =
                            orch_ask::chat_reply_message(&question_set, question_index, &text);
                        planner_messages.push(Message::user().with_text(reply));
                    }
                    orch_ask::AskOutcome::Cancelled => {
                        planner_messages.push(Message::user().with_text(
                            "The user declined to answer. Proceed using your own recommended options and produce the plan now.",
                        ));
                        force_final_plan = true;
                    }
                }
            }
        }
    };
    if !qa_rounds.is_empty() {
        plan_text = format!(
            "{}\n\n{}",
            orch_ask::qa_markdown_section(&qa_rounds),
            plan_text
        );
    }
    if plan_text.trim().is_empty() {
        anyhow::bail!("planner produced an empty plan");
    }
    let planner_context_limit = planner.get_context_limit(&planner_model).await.ok();
    record_phase(
        meta,
        "plan",
        0,
        "planner",
        planner_role,
        plan_usage.as_ref(),
        planner_context_limit,
        phase_started.elapsed().as_millis() as u64,
        None,
        None,
        Some(&plan_exemplar_injection),
        None,
    );

    persist_artifact(artifact_dir, run_id, "plan.md", &plan_text);
    println!(
        "  {}",
        console::style(format!("artifacts → .goose-orch/{}/", run_id)).dim()
    );

    Ok(PlanPhaseOutput {
        plan_text,
        planner,
        planner_model,
        planner_context_limit,
    })
}
