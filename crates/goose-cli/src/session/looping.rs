use anyhow::Result;
use console::{measure_text_width, style};
use goose::conversation::message::Message;
use goose_providers::conversation::token_usage::Usage;
use std::io::{IsTerminal, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::signal::ctrl_c;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use super::{ledger, output, CliSession};

pub(crate) const LOOP_USAGE: &str = "\
Usage: /loop <interval> [--max N] [--until-done] <prompt>
Accepted interval formats: 30s, 5m, 1h, 90 (plain seconds)
Example: /loop 30s check CI";

const LOOP_DONE_INSTRUCTION: &str = "\
If this recurring task is complete and further loop runs are unnecessary, end \
your reply with a final line containing exactly LOOP_DONE. Otherwise do not \
include LOOP_DONE.";

#[derive(Debug, Clone)]
pub(crate) struct LoopCommand {
    pub every: Duration,
    pub prompt: String,
    pub max_iterations: Option<u32>,
    pub until_done: bool,
}

#[derive(Debug, Clone)]
pub(super) enum ParsedLoopCommand {
    Start(LoopCommand),
    Stop,
}

#[derive(Debug, Clone, Copy)]
enum LoopEndReason {
    Stopped,
    Cancelled,
    MaxReached,
    Done,
    Replaced,
    Error,
}

impl LoopEndReason {
    fn label(self) -> &'static str {
        match self {
            LoopEndReason::Stopped => "stopped",
            LoopEndReason::Cancelled => "cancelled",
            LoopEndReason::MaxReached => "max reached",
            LoopEndReason::Done => "done",
            LoopEndReason::Replaced => "replaced",
            LoopEndReason::Error => "ended after error",
        }
    }
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

struct LoopRunStats {
    started_at: Instant,
    starting_usage: UsageSnapshot,
    iterations: u32,
}

impl LoopRunStats {
    fn new(starting_usage: UsageSnapshot) -> Self {
        Self {
            started_at: Instant::now(),
            starting_usage,
            iterations: 0,
        }
    }
}

enum LoopWaitOutcome {
    Continue,
    Stop,
    Replace(LoopCommand),
}

enum WaitInputEvent {
    Escape,
    Line(String),
}

#[derive(Debug, PartialEq, Eq)]
enum WaitInputClass {
    BareEsc,
    EscapeSequence { len: usize, complete: bool },
    Ordinary,
}

#[derive(Debug, PartialEq, Eq)]
enum WaitEditStep {
    None,
    Submit(String),
}

pub(crate) fn parse_interval(input: &str) -> std::result::Result<Duration, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(LOOP_USAGE.to_string());
    }

    let (number, unit) = if let Some(number) = input.strip_suffix('s') {
        (number, Some('s'))
    } else if let Some(number) = input.strip_suffix('m') {
        (number, Some('m'))
    } else if let Some(number) = input.strip_suffix('h') {
        (number, Some('h'))
    } else if input.chars().all(|c| c.is_ascii_digit()) {
        (input, None)
    } else {
        return Err(LOOP_USAGE.to_string());
    };

    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return Err(LOOP_USAGE.to_string());
    }

    let value = number.parse::<u64>().map_err(|_| LOOP_USAGE.to_string())?;
    if value == 0 {
        return Err(LOOP_USAGE.to_string());
    }

    let seconds = match unit {
        Some('h') => value.checked_mul(60 * 60),
        Some('m') => value.checked_mul(60),
        Some('s') | None => Some(value),
        _ => None,
    }
    .ok_or_else(|| LOOP_USAGE.to_string())?;

    Ok(Duration::from_secs(seconds))
}

pub(super) fn parse_loop_command_args(
    args: &str,
) -> std::result::Result<ParsedLoopCommand, String> {
    let args = args.trim();
    if args.eq_ignore_ascii_case("stop") {
        return Ok(ParsedLoopCommand::Stop);
    }
    if args.is_empty() {
        return Err(LOOP_USAGE.to_string());
    }

    let parts = shlex::split(args).ok_or_else(|| LOOP_USAGE.to_string())?;
    if parts.is_empty() {
        return Err(LOOP_USAGE.to_string());
    }

    let mut every = None;
    let mut max_iterations = None;
    let mut until_done = false;
    let mut prompt_parts = Vec::new();
    let mut prompt_started = false;
    let mut index = 0;

    while index < parts.len() {
        let part = &parts[index];
        if prompt_started {
            prompt_parts.push(part.clone());
            index += 1;
            continue;
        }

        match part.as_str() {
            "--until-done" => {
                until_done = true;
                index += 1;
            }
            "--max" => {
                index += 1;
                let Some(raw) = parts.get(index) else {
                    return Err(LOOP_USAGE.to_string());
                };
                let parsed = raw.parse::<u32>().map_err(|_| LOOP_USAGE.to_string())?;
                if parsed == 0 {
                    return Err(LOOP_USAGE.to_string());
                }
                max_iterations = Some(parsed);
                index += 1;
            }
            flag if flag.starts_with("--") => return Err(LOOP_USAGE.to_string()),
            value if every.is_none() => {
                every = Some(parse_interval(value)?);
                index += 1;
            }
            _ => {
                prompt_started = true;
                prompt_parts.push(part.clone());
                index += 1;
            }
        }
    }

    let prompt = prompt_parts.join(" ").trim().to_string();
    if prompt.is_empty() {
        return Err(LOOP_USAGE.to_string());
    }

    Ok(ParsedLoopCommand::Start(LoopCommand {
        every: every.ok_or_else(|| LOOP_USAGE.to_string())?,
        prompt,
        max_iterations,
        until_done,
    }))
}

pub(super) fn response_has_loop_done(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim() == "LOOP_DONE")
}

pub(super) fn append_until_done_instruction(prompt: &str) -> String {
    format!("{prompt}\n\n{LOOP_DONE_INSTRUCTION}")
}

pub(super) fn loop_status_label(iteration: u32, remaining: Duration) -> String {
    format!(
        "↻ loop #{} · next run in {}",
        iteration,
        format_interval(remaining)
    )
}

fn format_interval(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 && seconds.is_multiple_of(3600) {
        format!("{}h", seconds / 3600)
    } else if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn classify_wait_input(bytes: &[u8]) -> WaitInputClass {
    if bytes.first() != Some(&0x1b) {
        return WaitInputClass::Ordinary;
    }
    if bytes.len() == 1 {
        return WaitInputClass::BareEsc;
    }
    if bytes[1] == b'[' || bytes[1] == b'O' {
        let (tail_len, complete) = escape_sequence_tail_len(&bytes[2..]);
        return WaitInputClass::EscapeSequence {
            len: 2 + tail_len,
            complete,
        };
    }

    WaitInputClass::BareEsc
}

fn escape_sequence_tail_len(bytes: &[u8]) -> (usize, bool) {
    for (index, &byte) in bytes.iter().enumerate() {
        if (0x30..=0x3f).contains(&byte) || (0x20..=0x2f).contains(&byte) {
            continue;
        }
        return (index + 1, true);
    }

    (bytes.len(), false)
}

fn apply_wait_edit(line: &mut String, byte: u8) -> WaitEditStep {
    match byte {
        b'\n' | b'\r' => WaitEditStep::Submit(std::mem::take(line)),
        0x7f | 0x08 => {
            line.pop();
            WaitEditStep::None
        }
        b' '..=b'~' => {
            line.push(byte as char);
            WaitEditStep::None
        }
        _ => WaitEditStep::None,
    }
}

fn render_wait_status_line(label: &str, input: &str, width: usize) -> String {
    let line = if input.is_empty() {
        label.to_string()
    } else {
        format!("{label}  > {input}")
    };
    truncate_to_width(&line, width)
}

fn truncate_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut rendered = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let mut buf = [0; 4];
        let ch_width = measure_text_width(ch.encode_utf8(&mut buf));
        if used + ch_width > width {
            break;
        }
        rendered.push(ch);
        used += ch_width;
    }
    rendered
}

fn terminal_width() -> usize {
    let (_, cols) = console::Term::stdout().size();
    cols as usize
}

fn paint_wait_status_line(label: &str, input: &str) {
    let line = render_wait_status_line(label, input, terminal_width());
    print!("\r\x1b[2K{}", style(line).dim());
    let _ = std::io::stdout().flush();
}

fn clear_wait_status_line() {
    print!("\r\x1b[2K");
    let _ = std::io::stdout().flush();
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

fn loop_run_id() -> String {
    format!("loop-{:x}", ledger::now_ms())
}

impl CliSession {
    pub(crate) async fn headless_loop(&mut self, command: LoopCommand) -> Result<()> {
        let result = self.run_loop(command, false).await;
        self.agent
            .emit_hook(goose::hooks::HookEvent::SessionEnd, &self.session_id)
            .await;
        result
    }

    pub(super) async fn run_loop(&mut self, command: LoopCommand, interactive: bool) -> Result<()> {
        self.loop_active.store(true, Ordering::SeqCst);
        self.loop_stop_requested.store(false, Ordering::SeqCst);
        let result = self.run_loop_inner(command, interactive).await;
        self.loop_active.store(false, Ordering::SeqCst);
        self.loop_stop_requested.store(false, Ordering::SeqCst);
        self.set_loop_status(None);
        result
    }

    async fn run_loop_inner(&mut self, mut command: LoopCommand, interactive: bool) -> Result<()> {
        let mut run_id = loop_run_id();
        let mut stats = LoopRunStats::new(self.usage_snapshot().await);

        loop {
            let iteration = stats.iterations + 1;
            self.set_loop_status(Some(format!(
                "↻ loop #{} · running every {}",
                iteration,
                format_interval(command.every)
            )));
            self.render_loop_iteration_banner(iteration, &command);

            let iteration_started = Instant::now();
            let before_usage = self.usage_snapshot().await;
            let prompt = if command.until_done {
                append_until_done_instruction(&command.prompt)
            } else {
                command.prompt.clone()
            };
            let turn_cancel = CancellationToken::new();
            self.push_message(Message::user().with_text(&prompt));

            if interactive {
                output::run_status_hook("thinking");
                output::show_thinking();
            }
            let result = self
                .process_agent_response(interactive, turn_cancel.clone())
                .await;
            if interactive {
                output::hide_thinking();
            }

            let after_usage = self.usage_snapshot().await;
            let usage_delta = after_usage.delta_since(before_usage);
            stats.iterations += 1;
            let done = command.until_done
                && self
                    .last_assistant_text()
                    .as_deref()
                    .is_some_and(response_has_loop_done);
            self.append_loop_ledger(
                &run_id,
                stats.iterations,
                &command.prompt,
                usage_delta,
                iteration_started.elapsed(),
                done.then_some("LOOP_DONE"),
            )
            .await;

            if let Err(error) = result {
                self.render_loop_summary(&stats, LoopEndReason::Error).await;
                return Err(error);
            }

            if turn_cancel.is_cancelled() || self.loop_stop_requested.swap(false, Ordering::SeqCst)
            {
                self.render_loop_summary(&stats, LoopEndReason::Cancelled)
                    .await;
                return Ok(());
            }

            if done {
                self.render_loop_summary(&stats, LoopEndReason::Done).await;
                return Ok(());
            }

            if command.max_iterations == Some(stats.iterations) {
                self.render_loop_summary(&stats, LoopEndReason::MaxReached)
                    .await;
                return Ok(());
            }

            match self
                .wait_for_loop_interval(stats.iterations + 1, command.every, interactive)
                .await?
            {
                LoopWaitOutcome::Continue => {}
                LoopWaitOutcome::Stop => {
                    self.render_loop_summary(&stats, LoopEndReason::Stopped)
                        .await;
                    return Ok(());
                }
                LoopWaitOutcome::Replace(next) => {
                    self.render_loop_summary(&stats, LoopEndReason::Replaced)
                        .await;
                    command = next;
                    run_id = loop_run_id();
                    stats = LoopRunStats::new(self.usage_snapshot().await);
                    self.loop_stop_requested.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    fn render_loop_iteration_banner(&self, iteration: u32, command: &LoopCommand) {
        let mut details = format!(
            "↻ loop #{iteration} · every {}",
            format_interval(command.every)
        );
        if let Some(max) = command.max_iterations {
            details.push_str(&format!(" · max {max}"));
        }
        if command.until_done {
            details.push_str(" · until done");
        }
        println!("\n{}", style(details).cyan().bold());
    }

    async fn wait_for_loop_interval(
        &self,
        next_iteration: u32,
        every: Duration,
        interactive: bool,
    ) -> Result<LoopWaitOutcome> {
        let started = Instant::now();
        let initial_label = loop_status_label(next_iteration, every);
        self.set_loop_status(Some(initial_label.clone()));

        let mut sleep = Box::pin(tokio::time::sleep(every));
        if !interactive {
            println!("  {}", style(initial_label).dim());
            tokio::select! {
                _ = &mut sleep => return Ok(LoopWaitOutcome::Continue),
                _ = ctrl_c() => return Ok(LoopWaitOutcome::Stop),
            }
        }

        let mut input = LoopWaitStdin::enable();
        let repaint = input.is_some() && std::io::stdout().is_terminal();
        if repaint {
            paint_wait_status_line(&initial_label, "");
        } else {
            println!("  {}", style(&initial_label).dim());
        }

        let mut tick = tokio::time::interval(Duration::from_millis(150));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_remaining = every.as_secs().saturating_add(1);

        let outcome = 'wait: loop {
            tokio::select! {
                _ = &mut sleep => break 'wait LoopWaitOutcome::Continue,
                _ = ctrl_c() => break 'wait LoopWaitOutcome::Stop,
                _ = tick.tick() => {
                    let remaining = every.saturating_sub(started.elapsed());
                    let current_label = loop_status_label(next_iteration, remaining);
                    if remaining.as_secs() != last_remaining {
                        last_remaining = remaining.as_secs();
                        self.set_loop_status(Some(current_label.clone()));
                    }

                    let events = if let Some(input) = input.as_mut() {
                        input.poll_events()
                    } else {
                        Vec::new()
                    };

                    for event in events {
                        match event {
                            WaitInputEvent::Escape => break 'wait LoopWaitOutcome::Stop,
                            WaitInputEvent::Line(line) => {
                                if repaint {
                                    clear_wait_status_line();
                                }
                                match self.handle_loop_wait_line(&line).await {
                                    LoopWaitOutcome::Continue => {}
                                    other => break 'wait other,
                                }
                            }
                        }
                    }

                    if repaint {
                        let current_input = input
                            .as_ref()
                            .map(LoopWaitStdin::current_input)
                            .unwrap_or("");
                        paint_wait_status_line(&current_label, current_input);
                    }
                }
            }
        };

        if repaint {
            clear_wait_status_line();
        }

        Ok(outcome)
    }

    async fn handle_loop_wait_line(&self, line: &str) -> LoopWaitOutcome {
        let line = line.trim();
        if line.is_empty() {
            return LoopWaitOutcome::Continue;
        }
        if line == "/loop stop" {
            return LoopWaitOutcome::Stop;
        }
        if line == "/loop" || line.starts_with("/loop ") {
            match parse_loop_command_args(line.trim_start_matches("/loop").trim()) {
                Ok(ParsedLoopCommand::Start(command)) => return LoopWaitOutcome::Replace(command),
                Ok(ParsedLoopCommand::Stop) => return LoopWaitOutcome::Stop,
                Err(error) => output::render_error(&error),
            }
            return LoopWaitOutcome::Continue;
        }

        self.handle_live_command_during_wait(line).await;
        LoopWaitOutcome::Continue
    }

    fn set_loop_status(&self, status: Option<String>) {
        if let Ok(mut cache) = self.completion_cache.write() {
            cache.status_line = status;
        }
    }

    async fn usage_snapshot(&self) -> UsageSnapshot {
        self.get_session()
            .await
            .map(|session| UsageSnapshot::from_usage(&session.accumulated_usage))
            .unwrap_or_default()
    }

    pub(super) fn last_assistant_text(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == rmcp::model::Role::Assistant)
            .map(|message| message.as_concat_text())
    }

    async fn append_loop_ledger(
        &self,
        run_id: &str,
        iteration: u32,
        prompt: &str,
        usage_delta: UsageSnapshot,
        duration: Duration,
        verdict: Option<&str>,
    ) {
        let provider = match self.agent.provider().await {
            Ok(provider) => provider,
            Err(_) => return,
        };
        let provider_name = provider.get_name().to_string();
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
            phase: "loop".to_string(),
            cycle: iteration,
            role: "session".to_string(),
            provider: provider_name,
            config_model: model_config.model_name,
            reported_model: None,
            context_limit,
            input_tokens: Some(usage_delta.input),
            output_tokens: Some(usage_delta.output),
            duration_ms: duration.as_millis() as u64,
            verdict: verdict.map(ToString::to_string),
            permission_policy: None,
            permission_denials: None,
            task_preview: goose::utils::safe_truncate(prompt, 160),
            plan_exemplars_injected: None,
            plan_exemplar_run_ids: None,
            review_exemplars_injected: None,
            review_exemplar_run_ids: None,
            playbook_injected: None,
            arena_rank: None,
            arena_winner: None,
        });
    }

    async fn render_loop_summary(&self, stats: &LoopRunStats, reason: LoopEndReason) {
        let usage_delta = self
            .usage_snapshot()
            .await
            .delta_since(stats.starting_usage);
        println!(
            "\n  {}",
            style(format!(
                "↻ loop {} · {} iteration{} · {} elapsed · {} tokens",
                reason.label(),
                stats.iterations,
                if stats.iterations == 1 { "" } else { "s" },
                super::format_elapsed_time(stats.started_at.elapsed()),
                fmt_tokens(usage_delta.total),
            ))
            .dim()
        );
    }
}

#[cfg(unix)]
struct LoopWaitStdin {
    fd: i32,
    prev_flags: i32,
    prev_termios: libc::termios,
    buf: Vec<u8>,
    line: String,
    esc_deferred: bool,
    escape_sequence_pending: bool,
}

#[cfg(unix)]
impl LoopWaitStdin {
    fn enable() -> Option<Self> {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        let fd = stdin.as_raw_fd();
        let prev_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if prev_flags < 0 {
            return None;
        }

        let mut prev_termios = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut prev_termios) } < 0 {
            return None;
        }
        let mut raw = prev_termios;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
            return None;
        }

        if unsafe { libc::fcntl(fd, libc::F_SETFL, prev_flags | libc::O_NONBLOCK) } < 0 {
            let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &prev_termios) };
            return None;
        }

        Some(Self {
            fd,
            prev_flags,
            prev_termios,
            buf: Vec::new(),
            line: String::new(),
            esc_deferred: false,
            escape_sequence_pending: false,
        })
    }

    fn poll_events(&mut self) -> Vec<WaitInputEvent> {
        let mut tmp = [0u8; 1024];
        loop {
            let n =
                unsafe { libc::read(self.fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
            if n > 0 {
                self.buf.extend_from_slice(&tmp[..n as usize]);
                if (n as usize) < tmp.len() {
                    break;
                }
            } else {
                break;
            }
        }

        let mut events = Vec::new();
        loop {
            if self.escape_sequence_pending {
                let (len, complete) = escape_sequence_tail_len(&self.buf);
                if len == 0 {
                    break;
                }
                self.buf.drain(..len);
                self.escape_sequence_pending = !complete;
                if !complete {
                    break;
                }
                continue;
            }

            if self.buf.is_empty() {
                break;
            }

            match classify_wait_input(&self.buf) {
                WaitInputClass::BareEsc => {
                    if self.buf.len() == 1 && !self.esc_deferred {
                        self.esc_deferred = true;
                        break;
                    }
                    self.buf.drain(..1);
                    self.esc_deferred = false;
                    events.push(WaitInputEvent::Escape);
                }
                WaitInputClass::EscapeSequence { len, complete } => {
                    self.buf.drain(..len);
                    self.esc_deferred = false;
                    self.escape_sequence_pending = !complete;
                    if !complete {
                        break;
                    }
                }
                WaitInputClass::Ordinary => {
                    let byte = self.buf.remove(0);
                    self.esc_deferred = false;
                    if let WaitEditStep::Submit(line) = apply_wait_edit(&mut self.line, byte) {
                        events.push(WaitInputEvent::Line(line.trim().to_string()));
                    }
                }
            }
        }
        events
    }

    fn current_input(&self) -> &str {
        &self.line
    }
}

#[cfg(unix)]
impl Drop for LoopWaitStdin {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(self.fd, libc::F_SETFL, self.prev_flags);
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.prev_termios);
        }
    }
}

#[cfg(not(unix))]
struct LoopWaitStdin;

#[cfg(not(unix))]
impl LoopWaitStdin {
    fn enable() -> Option<Self> {
        None
    }

    fn poll_events(&mut self) -> Vec<WaitInputEvent> {
        Vec::new()
    }

    fn current_input(&self) -> &str {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loop_intervals_with_units_and_plain_seconds() {
        assert_eq!(parse_interval("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_interval("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_interval("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_interval("90").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn rejects_bad_loop_intervals_with_friendly_usage() {
        let err = parse_interval("soon").unwrap_err();

        assert!(err.contains("Accepted interval formats"));
        assert!(err.contains("/loop 30s check CI"));
    }

    #[test]
    fn detects_loop_done_only_on_final_marker_line() {
        assert!(response_has_loop_done("all clear\nLOOP_DONE"));
        assert!(response_has_loop_done("all clear\n\nLOOP_DONE\n"));
        assert!(!response_has_loop_done("LOOP_DONE\nmore work remains"));
        assert!(!response_has_loop_done("the text LOOP_DONE appears inline"));
    }

    #[test]
    fn formats_loop_status_label_for_input_hint() {
        assert_eq!(
            loop_status_label(3, Duration::from_secs(300)),
            "↻ loop #3 · next run in 5m"
        );
        assert_eq!(
            loop_status_label(4, Duration::from_secs(90)),
            "↻ loop #4 · next run in 90s"
        );
    }

    #[test]
    fn classifies_bare_escape_vs_escape_sequences() {
        assert_eq!(classify_wait_input(&[0x1b]), WaitInputClass::BareEsc);
        assert_eq!(
            classify_wait_input(&[0x1b, b'[', b'A']),
            WaitInputClass::EscapeSequence {
                len: 3,
                complete: true
            }
        );
        assert_eq!(
            classify_wait_input(&[0x1b, b'O', b'P']),
            WaitInputClass::EscapeSequence {
                len: 3,
                complete: true
            }
        );
        assert_eq!(
            classify_wait_input(&[0x1b, b'[', b'1', b';', b'2', b'A']),
            WaitInputClass::EscapeSequence {
                len: 6,
                complete: true
            }
        );
        assert_eq!(classify_wait_input(b"x"), WaitInputClass::Ordinary);
    }

    #[test]
    fn edits_wait_input_and_submits_lines() {
        let mut line = String::new();

        assert_eq!(apply_wait_edit(&mut line, b'/'), WaitEditStep::None);
        assert_eq!(apply_wait_edit(&mut line, b's'), WaitEditStep::None);
        assert_eq!(apply_wait_edit(&mut line, b't'), WaitEditStep::None);
        assert_eq!(line, "/st");

        assert_eq!(apply_wait_edit(&mut line, 0x7f), WaitEditStep::None);
        assert_eq!(line, "/s");

        assert_eq!(apply_wait_edit(&mut line, b'a'), WaitEditStep::None);
        assert_eq!(
            apply_wait_edit(&mut line, b'\r'),
            WaitEditStep::Submit("/sa".to_string())
        );
        assert!(line.is_empty());
    }

    #[test]
    fn renders_wait_status_line_with_input_and_width_limit() {
        assert_eq!(render_wait_status_line("next in 5s", "", 80), "next in 5s");
        assert_eq!(
            render_wait_status_line("next in 5s", "/status", 80),
            "next in 5s  > /status"
        );
        assert_eq!(render_wait_status_line("abcdef", "", 3), "abc");
    }
}
