use anyhow::Result;
use console::style;
use goose::config::{Config, GooseMode};
use goose::conversation::message::Message;
use goose::utils::safe_truncate;
use std::sync::atomic::Ordering;

use super::ledger;
use super::orchestrate::{build_role_provider, resolve_all_roles, RoleConfig};
use super::{output, CliSession};

fn conn_kind(provider_name: &str) -> &'static str {
    if provider_name.ends_with("-acp") {
        "ACP · subscription"
    } else {
        "API"
    }
}

fn fmt_tokens(n: Option<i32>) -> String {
    match n {
        Some(n) if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        Some(n) if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        Some(n) => n.to_string(),
        None => "-".to_string(),
    }
}

fn kv(label: &str, value: &str) {
    println!("  {:<14} {}", style(label).dim(), value);
}

fn section(title: &str) {
    println!("{}", style(title).cyan().bold());
}

/// Claude Code's own default model (from ~/.claude/settings.json), used to
/// annotate what claude-acp's "default" alias actually resolves to.
fn claude_default_model() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let content = std::fs::read_to_string(format!("{}/.claude/settings.json", home)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value.get("model")?.as_str().map(|s| s.to_string())
}

fn annotate_model(provider: &str, model: &str) -> String {
    if provider == "claude-acp" && model == "default" {
        if let Some(resolved) = claude_default_model() {
            return format!("{} {}", model, style(format!("(→ {})", resolved)).dim());
        }
    }
    model.to_string()
}

fn role_desc(role: &RoleConfig) -> String {
    let mut desc = format!(
        "{}/{}  {}",
        role.provider_name,
        annotate_model(&role.provider_name, &role.model),
        style(format!("[{}]", conn_kind(&role.provider_name))).dim()
    );
    match role.effort.as_deref() {
        Some(e) => desc.push_str(&format!("  effort={}", e)),
        None if role.provider_name.ends_with("-acp") => {
            desc.push_str(&format!("  {}", style("effort=agent-managed").dim()));
        }
        None => {}
    }
    desc
}

/// Apply a whitespace-separated roles spec ("planner=prov/model
/// implementer.effort=high cycles=2"). Returns human-readable errors for
/// tokens that could not be applied. Synchronous so key bindings can use it.
pub(super) fn apply_roles_spec(spec: &str) -> Vec<String> {
    let config = Config::global();
    let mut errors = Vec::new();
    for token in spec.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            errors.push(format!(
                "Invalid assignment '{}'. Use role=provider/model, <role>.effort=<level>, or cycles=<n>.",
                token
            ));
            continue;
        };
        let result: std::result::Result<(), String> = match key {
            "planner" | "implementer" | "reviewer" => match value.split_once('/') {
                Some((provider, model)) => {
                    let upper = key.to_uppercase();
                    config
                        .set_param(&format!("GOOSE_{}_PROVIDER", upper), provider)
                        .and_then(|_| config.set_param(&format!("GOOSE_{}_MODEL", upper), model))
                        .map_err(|e| e.to_string())
                }
                None => Err(format!("Invalid value '{}'. Use {}=<provider>/<model>.", value, key)),
            },
            "planner.effort" | "implementer.effort" | "reviewer.effort" | "effort" => {
                let role = key.strip_suffix(".effort").unwrap_or("implementer");
                let mut r = config
                    .set_param(&format!("GOOSE_{}_EFFORT", role.to_uppercase()), value)
                    .map_err(|e| e.to_string());
                // codex-acp reads reasoning effort through its own -c flag,
                // so mirror the implementer effort there as well.
                if r.is_ok() && role == "implementer" {
                    r = config
                        .set_param("GOOSE_CODEX_REASONING_EFFORT", value)
                        .map_err(|e| e.to_string());
                }
                r
            }
            "cycles" => match value.parse::<u32>() {
                Ok(n) if n >= 1 => config
                    .set_param("GOOSE_ORCH_MAX_CYCLES", n)
                    .map_err(|e| e.to_string()),
                _ => Err("cycles must be a positive integer".to_string()),
            },
            _ => Err(format!(
                "Unknown key '{}'. Valid: planner, implementer, reviewer, <role>.effort, effort, cycles.",
                key
            )),
        };
        if let Err(e) = result {
            errors.push(e);
        }
    }
    errors
}

/// Serialize the currently resolved roles into a spec string /preset can store.
pub(super) fn current_roles_spec() -> Result<String> {
    let roles = resolve_all_roles()?;
    let mut parts = Vec::new();
    for (name, role) in [
        ("planner", &roles.planner),
        ("implementer", &roles.implementer),
        ("reviewer", &roles.reviewer),
    ] {
        parts.push(format!("{}={}/{}", name, role.provider_name, role.model));
        if let Some(effort) = &role.effort {
            parts.push(format!("{}.effort={}", name, effort));
        }
    }
    Ok(parts.join(" "))
}

pub(super) type PresetMap = std::collections::BTreeMap<String, String>;

pub(super) fn load_presets() -> PresetMap {
    Config::global()
        .get_param::<PresetMap>("GOOSE_ROLE_PRESETS")
        .unwrap_or_default()
}

pub(super) fn save_presets(presets: &PresetMap) -> Result<()> {
    Config::global().set_param("GOOSE_ROLE_PRESETS", presets)?;
    Ok(())
}

/// Apply a named preset. Returns its spec on success. Synchronous so the
/// Shift+Tab cycler can call it from a key binding.
pub(super) fn apply_preset(name: &str) -> std::result::Result<String, String> {
    let presets = load_presets();
    let Some(spec) = presets.get(name) else {
        return Err(format!("Unknown preset '{}'", name));
    };
    let errors = apply_roles_spec(spec);
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let _ = Config::global().set_param("GOOSE_ACTIVE_PRESET", name);
    Ok(spec.clone())
}

fn print_roles_table() {
    match resolve_all_roles() {
        Ok(roles) => {
            kv("planner", &role_desc(&roles.planner));
            kv("implementer", &role_desc(&roles.implementer));
            kv("reviewer", &role_desc(&roles.reviewer));
            let max_cycles = Config::global()
                .get_param::<u32>("GOOSE_ORCH_MAX_CYCLES")
                .unwrap_or(3);
            kv("max cycles", &max_cycles.to_string());
        }
        Err(e) => kv("roles", &format!("unavailable: {}", e)),
    }
}

/// ExternalPrinter substitute for contexts where rustyline isn't active
/// (live commands issued while a turn is streaming).
pub(super) struct StdoutPrinter;

impl rustyline::ExternalPrinter for StdoutPrinter {
    fn print(&mut self, msg: String) -> rustyline::Result<()> {
        print!("{}", msg);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        Ok(())
    }
}

impl CliSession {
    /// Dispatch a slash command typed while a turn is streaming. Read-only
    /// commands only; anything else gets a hint instead of mutating state
    /// mid-turn.
    pub(super) async fn handle_live_command(&self, line: &str) {
        self.handle_live_command_inner(line, true, true).await;
    }

    pub(super) async fn handle_live_command_during_wait(&self, line: &str) {
        self.handle_live_command_inner(line, false, false).await;
    }

    async fn handle_live_command_inner(&self, line: &str, restore_thinking: bool, steer: bool) {
        let cmd = line.trim();
        if cmd.is_empty() {
            return;
        }
        output::hide_thinking();
        let result = if cmd == "/status" {
            self.handle_status().await
        } else if cmd == "/stats" {
            self.handle_stats().await
        } else if cmd == "/usage" {
            self.handle_usage().await
        } else if cmd == "/roles" {
            self.handle_roles(None).await
        } else if cmd == "/loop stop" {
            if self.loop_active.load(Ordering::SeqCst) {
                self.loop_stop_requested.store(true, Ordering::SeqCst);
                println!(
                    "\n  {}",
                    style("loop will stop after the current turn").yellow()
                );
            } else {
                println!("\n  {}", style("no active loop is running").dim());
            }
            Ok(())
        } else if cmd == "/goal" {
            self.render_goal_status().await
        } else if cmd == "/goal stop" {
            if self.goal_active.load(Ordering::SeqCst) {
                self.goal_stop_requested.store(true, Ordering::SeqCst);
                println!(
                    "\n  {}",
                    style("goal will stop after the current attempt").yellow()
                );
            } else {
                println!("\n  {}", style("no active goal loop is running").dim());
            }
            Ok(())
        } else if let Some(q) = cmd.strip_prefix("/btw") {
            self.handle_btw(q.trim().to_string(), Some(StdoutPrinter))
                .await
        } else if cmd.starts_with('/') {
            println!(
                "\n  {}",
                style(
                    "live commands while the agent runs: /goal stop /loop stop /status /stats /usage /roles /btw <question>"
                )
                .dim()
            );
            Ok(())
        } else if steer {
            self.sent_steers.lock().unwrap().push(cmd.to_string());
            self.agent
                .steer(&self.session_id, Message::user().with_text(cmd))
                .await;
            println!(
                "\n  {}",
                style("↪ steering — will be injected after the current tool call finishes").cyan()
            );
            Ok(())
        } else {
            println!(
                "\n  {}",
                style(
                    "loop wait commands: /loop stop /status /stats /usage /roles /btw <question>"
                )
                .dim()
            );
            Ok(())
        };
        if let Err(e) = result {
            output::render_error(&e.to_string());
        }
        if restore_thinking {
            output::show_thinking();
        }
    }

    pub(super) async fn handle_status(&self) -> Result<()> {
        let config = Config::global();
        let provider = self.agent.provider().await?;
        let provider_name = provider.get_name().to_string();
        let model_config = self
            .agent
            .model_config_for_session(&self.session_id)
            .await?;
        let effort = model_config
            .thinking_effort()
            .or(config.get_goose_thinking_effort());
        let mode = self.agent.goose_mode().await;
        let session = self.get_session().await?;

        println!();
        section("session");
        kv(
            "provider",
            &format!(
                "{}  {}",
                provider_name,
                style(format!("[{}]", conn_kind(&provider_name))).dim()
            ),
        );
        kv(
            "model",
            &annotate_model(&provider_name, &model_config.model_name),
        );
        kv(
            "effort",
            &effort
                .map(|e| e.to_string())
                .unwrap_or_else(|| "default".to_string()),
        );
        kv("mode", &mode.to_string());
        kv("directory", &session.working_dir.display().to_string());
        kv("messages", &session.message_count.to_string());
        let acc = &session.accumulated_usage;
        kv(
            "tokens",
            &format!(
                "in {} · out {} · total {}",
                fmt_tokens(acc.input_tokens),
                fmt_tokens(acc.output_tokens),
                fmt_tokens(acc.total_tokens)
            ),
        );
        if let Some(cost) = session.accumulated_cost {
            kv("cost", &format!("${:.4}", cost));
        }

        println!();
        section("orchestration roles (/orch · /roles)");
        print_roles_table();

        println!();
        section("subagents (delegate)");
        let sub_provider = config
            .get_param::<String>("GOOSE_SUBAGENT_PROVIDER")
            .unwrap_or_else(|_| provider_name.clone());
        let sub_model = config
            .get_param::<String>("GOOSE_SUBAGENT_MODEL")
            .unwrap_or_else(|_| model_config.model_name.clone());
        let sub_turns = config
            .get_param::<String>("GOOSE_SUBAGENT_MAX_TURNS")
            .unwrap_or_else(|_| "default".to_string());
        kv(
            "delegate to",
            &format!(
                "{}/{}  {}",
                sub_provider,
                sub_model,
                style(format!("[{}]", conn_kind(&sub_provider))).dim()
            ),
        );
        kv("max turns", &sub_turns);
        println!(
            "  {}",
            style("running subagent tool calls render inline as [subagent:<id>] lines").dim()
        );

        println!();
        self.display_context_usage().await?;
        Ok(())
    }

    pub(super) async fn handle_usage(&self) -> Result<()> {
        let session = self.get_session().await?;
        println!();
        section("usage (this session)");
        kv("messages", &session.message_count.to_string());
        let acc = &session.accumulated_usage;
        kv("input tokens", &fmt_tokens(acc.input_tokens));
        kv("output tokens", &fmt_tokens(acc.output_tokens));
        kv(
            "cache",
            &format!(
                "read {} · write {}",
                fmt_tokens(acc.cache_read_input_tokens),
                fmt_tokens(acc.cache_write_input_tokens)
            ),
        );
        kv("total", &fmt_tokens(acc.total_tokens));
        if let Some(cost) = session.accumulated_cost {
            kv("est. cost", &format!("${:.4}", cost));
        }
        println!();
        self.display_context_usage().await?;
        Ok(())
    }

    pub(super) async fn handle_btw<P>(&self, question: String, printer: Option<P>) -> Result<()>
    where
        P: rustyline::ExternalPrinter + Send + 'static,
    {
        let question = question.trim().to_string();
        if question.is_empty() {
            output::render_error(
                "Usage: /btw <question> — ask a side question without adding it to the session history.",
            );
            return Ok(());
        }

        let roles = resolve_all_roles()?;
        let role = roles.planner;
        println!(
            "{}",
            style(format!(
                "btw → {}/{} · running in the background, keep working — the answer will appear when ready",
                role.provider_name, role.model
            ))
            .dim()
        );

        let mut text = String::from(
            "This is a quick side question asked in the middle of a work session. Answer it directly and concisely. Do not modify any files.\n\n",
        );
        if let Some(context) = self
            .messages
            .messages()
            .iter()
            .rev()
            .find(|m| m.role == rmcp::model::Role::Assistant)
            .map(|m| m.as_concat_text())
        {
            if !context.trim().is_empty() {
                text.push_str(&format!(
                    "Recent session context (reference only):\n{}\n\n",
                    safe_truncate(&context, 4_000)
                ));
            }
        }
        text.push_str(&format!("Question:\n{}", question));

        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            let config = Config::global();
            let prev_mode = config.get_goose_mode().unwrap_or_default();
            let _ = config.set_goose_mode(GooseMode::Chat);
            let built = match std::env::current_dir() {
                Ok(current_dir) => build_role_provider(&role, &current_dir).await,
                Err(error) => Err(error.into()),
            };
            let _ = config.set_goose_mode(prev_mode);

            let result = async {
                let (provider, model_config) = built?;
                let (message, _usage) = goose::session_context::with_session_id(
                    Some(session_id),
                    provider.complete(
                        &model_config,
                        "You answer side questions concisely.",
                        &[Message::user().with_text(text)],
                        &[],
                    ),
                )
                .await?;
                Ok::<String, anyhow::Error>(message.as_concat_text())
            }
            .await;

            let rendered = match result {
                Ok(answer) => format!(
                    "\n{} {}\n{}\n",
                    console::style("●").cyan(),
                    console::style(format!("btw · {}", question)).dim(),
                    answer.trim()
                ),
                Err(e) => format!("\nbtw failed: {}\n", e),
            };
            match printer {
                Some(mut p) => {
                    let _ = p.print(rendered);
                }
                None => print!("{}", rendered),
            }
        });
        Ok(())
    }

    pub(super) async fn handle_stats(&self) -> Result<()> {
        let records = ledger::read_all();
        if records.is_empty() {
            println!(
                "\n  {}",
                style("No orchestration or goal runs recorded yet — run /orch or /goal first.")
                    .dim()
            );
            return Ok(());
        }

        struct Agg {
            phases: u32,
            runs: std::collections::BTreeSet<String>,
            in_tok: i64,
            out_tok: i64,
            dur_ms: u64,
            approved: u32,
            revised: u32,
        }
        let mut aggs: std::collections::BTreeMap<String, Agg> = Default::default();
        for r in &records {
            let key = format!("{} · {}/{}", r.role, r.provider, r.config_model);
            let agg = aggs.entry(key).or_insert(Agg {
                phases: 0,
                runs: Default::default(),
                in_tok: 0,
                out_tok: 0,
                dur_ms: 0,
                approved: 0,
                revised: 0,
            });
            agg.phases += 1;
            agg.runs.insert(r.run_id.clone());
            agg.in_tok += r.input_tokens.unwrap_or(0);
            agg.out_tok += r.output_tokens.unwrap_or(0);
            agg.dur_ms += r.duration_ms;
            match r.verdict.as_deref() {
                Some("APPROVED") => agg.approved += 1,
                Some("REVISE") => agg.revised += 1,
                _ => {}
            }
        }

        println!();
        section("stats · by role and model (all recorded orch/goal runs)");
        for (key, a) in &aggs {
            let avg_s = if a.phases > 0 {
                a.dur_ms as f64 / a.phases as f64 / 1000.0
            } else {
                0.0
            };
            let mut line = format!(
                "{} runs · {} phases · in {} / out {} tok · avg {:.1}s",
                a.runs.len(),
                a.phases,
                a.in_tok,
                a.out_tok,
                avg_s
            );
            if a.approved + a.revised > 0 {
                line.push_str(&format!(
                    " · {} approved / {} revised",
                    a.approved, a.revised
                ));
            }
            kv(key, &line);
        }

        if let Some(last) = records.last() {
            let last_run = last.run_id.clone();
            println!();
            section(&format!("last run · {}", last_run));
            for r in records.iter().filter(|r| r.run_id == last_run) {
                kv(
                    &format!("{} c{}", r.phase, r.cycle),
                    &format!(
                        "{} → {} · in {} / out {} · {:.1}s{}{}",
                        r.config_model,
                        r.reported_model.as_deref().unwrap_or("(unreported)"),
                        r.input_tokens.unwrap_or(0),
                        r.output_tokens.unwrap_or(0),
                        r.duration_ms as f64 / 1000.0,
                        r.verdict
                            .as_deref()
                            .map(|v| format!(" · {}", v))
                            .unwrap_or_default(),
                        r.context_limit
                            .map(|c| format!(" · ctx-limit {}k", c / 1000))
                            .unwrap_or_default()
                    ),
                );
            }
            kv(
                "task",
                &records
                    .iter()
                    .rfind(|r| r.run_id == last_run)
                    .map(|r| r.task_preview.clone())
                    .unwrap_or_default(),
            );
        }
        println!(
            "\n  {}",
            style(format!("ledger: {}", ledger::path_display())).dim()
        );
        Ok(())
    }

    pub(super) async fn handle_bash(&mut self, cmd: String) -> Result<()> {
        println!("{} {}", style("$").cyan().bold(), style(&cmd).bold());
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stdout.trim().is_empty() {
            print!("{}", stdout);
        }
        if !stderr.trim().is_empty() {
            eprint!("{}", stderr);
        }
        let code = out.status.code().unwrap_or(-1);
        println!(
            "  {}",
            style(format!(
                "(exit {} · output added to conversation context)",
                code
            ))
            .dim()
        );
        let mut combined = format!("{}{}", stdout, stderr);
        combined = safe_truncate(&combined, 8_000);
        self.push_message(Message::user().with_text(format!(
            "I ran this shell command myself:\n$ {}\n(exit {})\n```\n{}\n```\nNo response needed — this is context for our conversation.",
            cmd, code, combined
        )));
        Ok(())
    }

    pub(super) async fn handle_remember(&self, note: String) -> Result<()> {
        let note = note.trim().to_string();
        if note.is_empty() {
            output::render_error("Usage: /remember <note> — appends to this project's .goosehints");
            return Ok(());
        }
        let path = std::path::Path::new(".goosehints");
        let mut content = std::fs::read_to_string(path).unwrap_or_default();
        if content.is_empty() {
            content.push_str("# Project notes\n");
        }
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("- {}\n", note));
        std::fs::write(path, content)?;
        println!(
            "\n  {} {}",
            style("✔ remembered in .goosehints:").green(),
            style(note).dim()
        );
        Ok(())
    }

    pub(super) async fn handle_preset(&self, args: Option<String>) -> Result<()> {
        let args = args.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let mut presets = load_presets();

        match args.as_deref() {
            Some(rest) if rest.starts_with("save ") || rest == "save" => {
                let name = rest.strip_prefix("save").unwrap_or("").trim();
                if name.is_empty() {
                    output::render_error("Usage: /preset save <name>");
                    return Ok(());
                }
                let spec = current_roles_spec()?;
                presets.insert(name.to_string(), spec.clone());
                save_presets(&presets)?;
                let _ = Config::global().set_param("GOOSE_ACTIVE_PRESET", name);
                println!(
                    "\n  {} {}",
                    style(format!("✔ preset '{}' saved:", name)).green(),
                    style(spec).dim()
                );
            }
            Some(rest) if rest.starts_with("delete ") => {
                let name = rest.strip_prefix("delete").unwrap_or("").trim();
                if presets.remove(name).is_some() {
                    save_presets(&presets)?;
                    println!(
                        "\n  {}",
                        style(format!("✔ preset '{}' deleted", name)).green()
                    );
                } else {
                    output::render_error(&format!("Unknown preset '{}'", name));
                }
            }
            Some(name) => match apply_preset(name) {
                Ok(_) => {
                    println!(
                        "\n  {}",
                        style(format!("✔ preset '{}' applied", name)).green()
                    );
                    print_roles_table();
                }
                Err(e) => output::render_error(&e),
            },
            None => {
                if presets.is_empty() {
                    println!(
                        "\n  {}",
                        style("No presets yet. Save the current roles with: /preset save <name>")
                            .dim()
                    );
                    return Ok(());
                }
                let active = Config::global()
                    .get_param::<String>("GOOSE_ACTIVE_PRESET")
                    .unwrap_or_default();
                if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                    let mut select =
                        cliclack::select("Pick a preset (Shift+Tab cycles at the prompt)");
                    for (name, spec) in &presets {
                        let label = if *name == active {
                            format!("{} (active)", name)
                        } else {
                            name.clone()
                        };
                        select = select.item(name.clone(), label, safe_truncate(spec, 60));
                    }
                    if let Ok(choice) = select.interact() {
                        match apply_preset(&choice) {
                            Ok(_) => {
                                println!(
                                    "\n  {}",
                                    style(format!("✔ preset '{}' applied", choice)).green()
                                );
                                print_roles_table();
                            }
                            Err(e) => output::render_error(&e),
                        }
                    }
                } else {
                    for (name, spec) in &presets {
                        kv(name, spec);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) async fn handle_roles(&self, spec: Option<String>) -> Result<()> {
        let spec = spec.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        if let Some(spec) = spec {
            for err in apply_roles_spec(&spec) {
                output::render_error(&err);
            }
        }

        println!();
        section("orchestration roles (/orch)");
        print_roles_table();
        println!(
            "  {}",
            style("change with: /roles planner=claude-acp/default implementer=codex-acp/gpt-5.5 implementer.effort=high cycles=3")
                .dim()
        );
        Ok(())
    }
}
