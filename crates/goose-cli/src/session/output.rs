use anstream::println;
use bat::WrappingMode;
use console::{measure_text_width, style, Color, Term};
use goose::config::Config;
use goose::conversation::message::{
    ActionRequiredData, Message, MessageContent, SystemNotificationContent, SystemNotificationType,
    ToolRequest, ToolResponse,
};
use goose::providers::canonical::maybe_get_canonical_model;
#[cfg(target_os = "windows")]
use goose::subprocess::SubprocessExt;
use goose::utils::safe_truncate;
use goose_providers::conversation::token_usage::Usage;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rmcp::model::{CallToolRequestParams, JsonObject, PromptArgument};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Error, IsTerminal, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicU32, AtomicU8, Ordering},
    Mutex, OnceLock,
};
use std::time::{Duration, Instant};

use super::streaming_buffer::MarkdownBuffer;

pub const DEFAULT_MIN_PRIORITY: f32 = 0.0;
pub const DEFAULT_CLI_LIGHT_THEME: &str = "GitHub";
pub const DEFAULT_CLI_DARK_THEME: &str = "zenburn";

// Re-export theme for use in main
#[derive(Clone, Copy)]
pub enum Theme {
    Light,
    Dark,
    Ansi,
}

impl Theme {
    fn as_str(&self) -> String {
        match self {
            Theme::Light => Config::global()
                .get_param::<String>("GOOSE_CLI_LIGHT_THEME")
                .unwrap_or(DEFAULT_CLI_LIGHT_THEME.to_string()),
            Theme::Dark => Config::global()
                .get_param::<String>("GOOSE_CLI_DARK_THEME")
                .unwrap_or(DEFAULT_CLI_DARK_THEME.to_string()),
            Theme::Ansi => "base16".to_string(),
        }
    }

    fn from_config_str(val: &str) -> Self {
        if val.eq_ignore_ascii_case("light") {
            Theme::Light
        } else if val.eq_ignore_ascii_case("ansi") {
            Theme::Ansi
        } else {
            Theme::Dark
        }
    }

    fn as_config_string(&self) -> String {
        match self {
            Theme::Light => "light".to_string(),
            Theme::Dark => "dark".to_string(),
            Theme::Ansi => "ansi".to_string(),
        }
    }
}

thread_local! {
    static CURRENT_THEME: RefCell<Theme> = RefCell::new(
        std::env::var("GOOSE_CLI_THEME").ok()
            .map(|val| Theme::from_config_str(&val))
            .unwrap_or_else(||
                Config::global().get_param::<String>("GOOSE_CLI_THEME").ok()
                    .map(|val| Theme::from_config_str(&val))
                    .unwrap_or(Theme::Ansi)
            )
    );
    static SHOW_FULL_TOOL_OUTPUT: RefCell<bool> = RefCell::new(
        Config::global().get_param::<bool>("GOOSE_SHOW_FULL_OUTPUT").unwrap_or(false)
    );
    static RESPONSE_BULLET_SHOWN: RefCell<bool> = const { RefCell::new(false) };
    static THINKING_CONTEXT: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveRole {
    Planner,
    Implementer,
    Reviewer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveRoleStatus {
    pub role: ActiveRole,
    pub cycle: Option<(u32, u32)>,
}

const ACTIVE_ROLE_NONE: u8 = 0;
const ACTIVE_ROLE_PLANNER: u8 = 1;
const ACTIVE_ROLE_IMPLEMENTER: u8 = 2;
const ACTIVE_ROLE_REVIEWER: u8 = 3;
const MAX_TRACKED_TOOL_CALLS: usize = 128;
const LONG_TOOL_CALL_DURATION: Duration = Duration::from_secs(3);
const THINKING_STATUS_REFRESH: Duration = Duration::from_secs(1);
/// While the model is actively streaming output, redrawing the spinner would
/// paint its status label into the middle of the streamed line. Suppress the
/// periodic spinner refresh for this long after the last streamed output so it
/// only reappears once output pauses (tool waits, end of turn).
const STATUS_OUTPUT_SUPPRESS_WINDOW: Duration = Duration::from_millis(2000);

static ACTIVE_ROLE: AtomicU8 = AtomicU8::new(ACTIVE_ROLE_NONE);
static ACTIVE_ROLE_CYCLE: AtomicU32 = AtomicU32::new(0);
static ACTIVE_ROLE_MAX_CYCLES: AtomicU32 = AtomicU32::new(0);
static TOOL_STARTED: OnceLock<Mutex<HashMap<String, RunningTool>>> = OnceLock::new();
static PHASE_PROGRESS: OnceLock<Mutex<Option<PhaseProgress>>> = OnceLock::new();

#[derive(Clone)]
struct RunningTool {
    started_at: Instant,
    summary: String,
}

struct PhaseProgress {
    label: String,
    cycle: Option<(u32, u32)>,
    started_at: Instant,
    last_activity: Instant,
    cadence: Duration,
    tool_calls: u32,
    last_summary: Option<String>,
}

pub fn set_active_role(role: Option<ActiveRole>) {
    set_active_role_status(role.map(|role| ActiveRoleStatus { role, cycle: None }));
}

pub fn set_active_role_status(status: Option<ActiveRoleStatus>) {
    let value = match status.map(|status| status.role) {
        Some(ActiveRole::Planner) => ACTIVE_ROLE_PLANNER,
        Some(ActiveRole::Implementer) => ACTIVE_ROLE_IMPLEMENTER,
        Some(ActiveRole::Reviewer) => ACTIVE_ROLE_REVIEWER,
        None => ACTIVE_ROLE_NONE,
    };
    ACTIVE_ROLE.store(value, Ordering::Relaxed);
    let (cycle, max_cycles) = status.and_then(|status| status.cycle).unwrap_or((0, 0));
    ACTIVE_ROLE_CYCLE.store(cycle, Ordering::Relaxed);
    ACTIVE_ROLE_MAX_CYCLES.store(max_cycles, Ordering::Relaxed);
    refresh_thinking_status();
}

fn active_role() -> Option<ActiveRole> {
    active_role_status().map(|status| status.role)
}

fn active_role_status() -> Option<ActiveRoleStatus> {
    match ACTIVE_ROLE.load(Ordering::Relaxed) {
        ACTIVE_ROLE_PLANNER => Some(ActiveRole::Planner),
        ACTIVE_ROLE_IMPLEMENTER => Some(ActiveRole::Implementer),
        ACTIVE_ROLE_REVIEWER => Some(ActiveRole::Reviewer),
        _ => None,
    }
    .map(|role| {
        let cycle = ACTIVE_ROLE_CYCLE.load(Ordering::Relaxed);
        let max_cycles = ACTIVE_ROLE_MAX_CYCLES.load(Ordering::Relaxed);
        let cycle = (cycle > 0 && max_cycles > 0).then_some((cycle, max_cycles));
        ActiveRoleStatus { role, cycle }
    })
}

fn role_color(role: ActiveRole) -> Color {
    match role {
        ActiveRole::Planner => Color::Cyan,
        ActiveRole::Implementer => Color::Yellow,
        ActiveRole::Reviewer => Color::Magenta,
    }
}

fn active_or_default_color(default: Color) -> Color {
    active_role().map(role_color).unwrap_or(default)
}

fn tool_started() -> &'static Mutex<HashMap<String, RunningTool>> {
    TOOL_STARTED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn phase_progress() -> &'static Mutex<Option<PhaseProgress>> {
    PHASE_PROGRESS.get_or_init(|| Mutex::new(None))
}

pub fn begin_phase_progress(label: &str, cycle: Option<(u32, u32)>, cadence: Duration) {
    let Ok(mut progress) = phase_progress().lock() else {
        return;
    };
    if cadence.is_zero() {
        *progress = None;
        return;
    }

    let now = Instant::now();
    *progress = Some(PhaseProgress {
        label: label.to_string(),
        cycle,
        started_at: now,
        last_activity: now,
        cadence,
        tool_calls: 0,
        last_summary: None,
    });
}

pub fn end_phase_progress() {
    if let Ok(mut progress) = phase_progress().lock() {
        *progress = None;
    }
}

fn note_phase_activity() {
    if let Ok(mut progress) = phase_progress().lock() {
        if let Some(progress) = progress.as_mut() {
            progress.last_activity = Instant::now();
        }
    }
}

fn note_phase_tool_start(started_at: Instant, summary: &str) {
    if let Ok(mut progress) = phase_progress().lock() {
        if let Some(progress) = progress.as_mut() {
            progress.tool_calls = progress.tool_calls.saturating_add(1);
            progress.last_summary = Some(summary.to_string());
            progress.last_activity = started_at;
        }
    }
}

pub fn phase_progress_tick() {
    let snapshot = {
        let Ok(mut progress) = phase_progress().lock() else {
            return;
        };
        let Some(progress) = progress.as_mut() else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(progress.last_activity) < progress.cadence {
            return;
        }
        progress.last_activity = now;
        (
            progress.label.clone(),
            progress.cycle,
            now.duration_since(progress.started_at),
            progress.tool_calls,
            progress.last_summary.clone(),
        )
    };

    let (label, cycle, elapsed, tool_calls, last_summary) = snapshot;
    let content = format_phase_progress(PhaseProgressInput {
        label: &label,
        cycle,
        elapsed,
        tool_calls,
        last_summary: last_summary.as_deref(),
        terminal_width: thinking_status_width(),
    });
    hide_thinking();
    println!("  {}", style(format!("⋯ {content}")).dim());
    let _ = std::io::stdout().flush();
}

/// Spinner label showing which model is currently in control
/// (e.g. "claude-acp/default working…"). None falls back to fun messages.
pub fn set_thinking_context(context: Option<String>) {
    THINKING_CONTEXT.with(|c| *c.borrow_mut() = context);
    refresh_thinking_status();
}

fn get_thinking_context() -> Option<String> {
    THINKING_CONTEXT.with(|c| c.borrow().clone())
}

pub struct ThinkingStatusLabelInput<'a> {
    pub base: &'a str,
    pub elapsed: Duration,
    pub role: Option<ActiveRoleStatus>,
    pub running_tools: &'a [String],
    pub terminal_width: Option<usize>,
    pub hint: Option<&'a str>,
}

pub fn build_thinking_status_label(input: ThinkingStatusLabelInput<'_>) -> String {
    let mut label = String::new();
    if let Some(role) = input.role {
        label.push_str(&format_active_role_status(role));
        label.push_str(" · ");
    }
    label.push_str(input.base);
    label.push_str(" for ");
    label.push_str(&format_elapsed(input.elapsed));

    match input.running_tools.len() {
        0 => {}
        1 => {
            label.push_str(" · ");
            label.push_str(&input.running_tools[0]);
            label.push_str(" running");
        }
        count => {
            label.push_str(" · ");
            label.push_str(&count.to_string());
            label.push_str(" tools running");
        }
    }

    if let Some(hint) = input.hint {
        label.push_str("  ");
        label.push_str(hint);
    }

    match input.terminal_width {
        Some(width) => truncate_to_display_width(&label, width),
        None => label,
    }
}

struct PhaseProgressInput<'a> {
    label: &'a str,
    cycle: Option<(u32, u32)>,
    elapsed: Duration,
    tool_calls: u32,
    last_summary: Option<&'a str>,
    terminal_width: Option<usize>,
}

fn format_phase_progress(input: PhaseProgressInput<'_>) -> String {
    let mut label = input.label.to_string();
    if let Some((cycle, max_cycles)) = input.cycle {
        label.push_str(&format!(" c{cycle}/{max_cycles}"));
    }
    label.push_str(" · ");
    label.push_str(&format_elapsed(input.elapsed));

    if input.tool_calls > 0 {
        label.push_str(&format!(
            " · {} tool call{}",
            input.tool_calls,
            if input.tool_calls == 1 { "" } else { "s" }
        ));
        if let Some(last_summary) = input.last_summary {
            label.push_str(" · last: ");
            label.push_str(last_summary);
        }
    } else {
        label.push_str(" · working…");
    }

    match input.terminal_width {
        Some(width) => truncate_to_display_width(&label, width),
        None => label,
    }
}

fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn format_active_role_status(status: ActiveRoleStatus) -> String {
    let role = match status.role {
        ActiveRole::Planner => "planner",
        ActiveRole::Implementer => "implementer",
        ActiveRole::Reviewer => "reviewer",
    };
    match status.cycle {
        Some((cycle, max_cycles)) => format!("{role} c{cycle}/{max_cycles}"),
        None => role.to_string(),
    }
}

fn truncate_to_display_width(text: &str, max_width: usize) -> String {
    if measure_text_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let suffix = "...";
    let suffix_width = measure_text_width(suffix);
    if max_width <= suffix_width {
        return ".".repeat(max_width);
    }

    let mut output = String::new();
    for ch in text.chars() {
        let char_width = measure_text_width(&ch.to_string());
        if measure_text_width(&output) + char_width + suffix_width > max_width {
            break;
        }
        output.push(ch);
    }
    output.push_str(suffix);
    output
}

fn thinking_status_width() -> Option<usize> {
    Term::stdout()
        .size_checked()
        .map(|(_height, width)| (width as usize).saturating_sub(4))
}

/// Reset the per-response bullet so the next assistant text block gets a fresh `●`.
pub fn reset_response_bullet() {
    RESPONSE_BULLET_SHOWN.with(|s| *s.borrow_mut() = false);
}

pub fn render_steer_injected(text: &str) {
    hide_thinking();
    println!(
        "\n{} {}",
        style("↪ steering:").cyan().bold(),
        style(text).dim()
    );
    let _ = std::io::stdout().flush();
}

fn print_response_bullet_once() {
    RESPONSE_BULLET_SHOWN.with(|s| {
        let mut shown = s.borrow_mut();
        if !*shown {
            let color = active_or_default_color(Color::White);
            println!("\n{}", style("●").fg(color).bold());
            *shown = true;
        }
    });
}

pub fn set_theme(theme: Theme) {
    let config = Config::global();
    config
        .set_param("GOOSE_CLI_THEME", theme.as_config_string())
        .expect("Failed to set theme");
    CURRENT_THEME.with(|t| *t.borrow_mut() = theme);

    let config = Config::global();
    let theme_str = match theme {
        Theme::Light => "light",
        Theme::Dark => "dark",
        Theme::Ansi => "ansi",
    };

    if let Err(e) = config.set_param("GOOSE_CLI_THEME", theme_str) {
        eprintln!("Failed to save theme setting to config: {}", e);
    }
}

pub fn get_theme() -> Theme {
    CURRENT_THEME.with(|t| *t.borrow())
}

pub fn toggle_full_tool_output() -> bool {
    SHOW_FULL_TOOL_OUTPUT.with(|s| {
        let mut val = s.borrow_mut();
        *val = !*val;
        *val
    })
}

pub fn get_show_full_tool_output() -> bool {
    SHOW_FULL_TOOL_OUTPUT.with(|s| *s.borrow())
}

// Simple wrapper around spinner to manage its state
#[derive(Default)]
pub struct ThinkingIndicator {
    spinner: Option<cliclack::ProgressBar>,
    turn_started_at: Option<Instant>,
    dynamic_message: Option<String>,
    fallback_message: Option<String>,
    last_message: Option<String>,
}

impl ThinkingIndicator {
    fn begin_fresh_turn(&mut self) {
        self.turn_started_at = Some(Instant::now());
        self.dynamic_message = None;
        self.fallback_message = None;
        self.last_message = None;
        clear_running_tools();
    }

    fn begin_turn(&mut self) {
        if self.turn_started_at.is_none() {
            self.begin_fresh_turn();
        }
    }

    fn finish_turn(&mut self) {
        self.hide();
        self.turn_started_at = None;
        self.dynamic_message = None;
        self.fallback_message = None;
        self.last_message = None;
        clear_running_tools();
    }

    fn set_base_message(&mut self, message: String) {
        self.dynamic_message = Some(message);
        self.refresh();
    }

    pub fn show(&mut self) {
        self.begin_turn();
        let message = self.current_message();
        if let Some(spinner) = self.spinner.as_mut() {
            if self.last_message.as_deref() != Some(message.as_str()) {
                spinner.set_message(message.clone());
                self.last_message = Some(message);
            }
            return;
        }

        let spinner = cliclack::spinner();
        spinner.start(message.clone());
        self.spinner = Some(spinner);
        self.last_message = Some(message);
    }

    fn refresh(&mut self) {
        if self.turn_started_at.is_none() {
            return;
        }
        if self.spinner.is_none() {
            self.show();
            return;
        }
        let message = self.current_message();
        if self.last_message.as_deref() == Some(message.as_str()) {
            return;
        }
        if let Some(spinner) = self.spinner.as_mut() {
            spinner.set_message(message.clone());
            self.last_message = Some(message);
        }
    }

    fn current_message(&mut self) -> String {
        let base = self.current_base_message();
        let elapsed = self
            .turn_started_at
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let running_tools = running_tool_summaries();
        let hint = "(Ctrl+C to interrupt)";
        let terminal_width = thinking_status_width();
        let hint_width = measure_text_width("  ") + measure_text_width(hint);
        let status_width =
            terminal_width.and_then(|width| (width > hint_width + 8).then_some(width - hint_width));
        let status_width = status_width.or(terminal_width.filter(|width| *width <= hint_width + 8));
        let role_status = active_role_status();
        let status = build_thinking_status_label(ThinkingStatusLabelInput {
            base: &base,
            elapsed,
            role: role_status,
            running_tools: &running_tools,
            terminal_width: status_width,
            hint: None,
        });

        let status = role_status
            .map(|role| style(status.clone()).fg(role_color(role.role)).to_string())
            .unwrap_or(status);

        if terminal_width.is_some() && status_width == terminal_width {
            status
        } else {
            format!("{}  {}", status, style(hint).dim())
        }
    }

    fn current_base_message(&mut self) -> String {
        if let Some(message) = &self.dynamic_message {
            return message.clone();
        }
        if let Some(context) = get_thinking_context() {
            return context;
        }
        if let Some(message) = &self.fallback_message {
            return message.clone();
        }
        let message = if Config::global()
            .get_param("RANDOM_THINKING_MESSAGES")
            .unwrap_or(true)
        {
            format!("{}...", super::thinking::get_random_thinking_message())
        } else {
            "Thinking...".to_string()
        };
        self.fallback_message = Some(message.clone());
        message
    }

    pub fn hide(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.stop("");
        }
        self.last_message = None;
    }

    pub fn is_shown(&self) -> bool {
        self.spinner.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct PromptInfo {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Option<Vec<PromptArgument>>,
    pub extension: Option<String>,
}

// Global thinking indicator
thread_local! {
    static THINKING: RefCell<ThinkingIndicator> = RefCell::new(ThinkingIndicator::default());
    static LAST_STREAMED_OUTPUT: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

/// Record that streamed output was just written to the terminal, so the next
/// periodic status refresh knows not to redraw the spinner over it.
pub fn note_streamed_output() {
    if std::io::stdout().is_terminal() {
        LAST_STREAMED_OUTPUT.with(|t| *t.borrow_mut() = Some(Instant::now()));
    }
}

fn should_suppress_status(last_output: Option<Instant>, now: Instant, window: Duration) -> bool {
    matches!(last_output, Some(at) if now.saturating_duration_since(at) < window)
}

pub fn show_thinking() {
    if std::io::stdout().is_terminal() {
        THINKING.with(|t| t.borrow_mut().show());
    }
}

pub fn hide_thinking() {
    if std::io::stdout().is_terminal() {
        THINKING.with(|t| t.borrow_mut().hide());
    }
}

pub struct ThinkingTurnGuard {
    active: bool,
}

impl Drop for ThinkingTurnGuard {
    fn drop(&mut self) {
        if self.active {
            finish_thinking_turn();
        }
    }
}

pub fn begin_thinking_turn() -> ThinkingTurnGuard {
    if std::io::stdout().is_terminal() {
        THINKING.with(|t| t.borrow_mut().begin_fresh_turn());
    }
    ThinkingTurnGuard { active: true }
}

pub fn finish_thinking_turn() {
    if std::io::stdout().is_terminal() {
        THINKING.with(|t| t.borrow_mut().finish_turn());
        LAST_STREAMED_OUTPUT.with(|t| *t.borrow_mut() = None);
    } else {
        clear_running_tools();
    }
}

pub fn refresh_thinking_status() {
    if std::io::stdout().is_terminal() {
        let suppress = LAST_STREAMED_OUTPUT.with(|t| {
            should_suppress_status(*t.borrow(), Instant::now(), STATUS_OUTPUT_SUPPRESS_WINDOW)
        });
        if suppress {
            return;
        }
        THINKING.with(|t| t.borrow_mut().refresh());
    }
}

pub fn thinking_status_refresh_interval() -> Duration {
    THINKING_STATUS_REFRESH
}

pub fn run_status_hook(status: &str) {
    if let Ok(hook) = Config::global().get_param::<String>("GOOSE_STATUS_HOOK") {
        let status = status.to_string();
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            let result = std::process::Command::new("cmd")
                .arg("/C")
                .arg(format!("{} {}", hook, status))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .set_no_window()
                .status();

            #[cfg(not(target_os = "windows"))]
            let result = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{} {}", hook, status))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            let _ = result;
        });
    }
}

pub fn is_showing_thinking() -> bool {
    THINKING.with(|t| t.borrow().is_shown())
}

pub fn set_thinking_message(s: &str) {
    if std::io::stdout().is_terminal() {
        THINKING.with(|t| t.borrow_mut().set_base_message(s.to_owned()));
    }
}

pub fn render_message(message: &Message, debug: bool) {
    let theme = get_theme();

    for content in &message.content {
        match content {
            MessageContent::ActionRequired(action) => match &action.data {
                ActionRequiredData::ToolConfirmation { tool_name, .. } => {
                    println!("action_required(tool_confirmation): {}", tool_name)
                }
                ActionRequiredData::Elicitation { message, .. } => {
                    println!("action_required(elicitation): {}", message)
                }
                ActionRequiredData::ElicitationResponse { id, .. } => {
                    println!("action_required(elicitation_response): {}", id)
                }
            },
            MessageContent::Text(text) => print_markdown(&text.text, theme),
            MessageContent::ToolRequest(req) => render_tool_request(req, theme, debug),
            MessageContent::ToolResponse(resp) => render_tool_response(resp, debug),
            MessageContent::Image(image) => {
                println!("Image: [data: {}, type: {}]", image.data, image.mime_type);
            }
            MessageContent::Thinking(t) => render_thinking(&t.thinking, theme),
            MessageContent::RedactedThinking(_) => {
                println!("\n{}", style("Thinking:").dim().italic());
                print_markdown("Thinking was redacted", theme);
            }
            MessageContent::SystemNotification(notification) => {
                match notification.notification_type {
                    SystemNotificationType::ThinkingMessage => {
                        show_thinking();
                        set_thinking_message(&notification.msg);
                    }
                    SystemNotificationType::InlineMessage => {
                        hide_thinking();
                        println!("\n{}", style(&notification.msg).yellow());
                    }
                    SystemNotificationType::CreditsExhausted => {
                        render_credits_exhausted_notification(notification);
                    }
                }
            }
            _ => {
                eprintln!("WARNING: Message content type could not be rendered");
            }
        }
    }

    let _ = std::io::stdout().flush();
}

/// Render a streaming message, using a buffer to accumulate text content
/// and only render when markdown constructs are complete.
pub fn render_message_streaming(
    message: &Message,
    buffer: &mut MarkdownBuffer,
    thinking_header_shown: &mut bool,
    debug: bool,
) {
    note_phase_activity();
    let theme = get_theme();

    for content in &message.content {
        if !matches!(content, MessageContent::Thinking(_)) {
            if *thinking_header_shown {
                println!();
            }
            *thinking_header_shown = false;
        }

        match content {
            MessageContent::Text(text) => {
                print_response_bullet_once();
                if let Some(safe_content) = buffer.push(&text.text) {
                    print_markdown(&safe_content, theme);
                }
            }
            MessageContent::ToolRequest(req) => {
                flush_markdown_buffer(buffer, theme);
                reset_response_bullet();
                render_tool_request(req, theme, debug);
            }
            MessageContent::ToolResponse(resp) => {
                flush_markdown_buffer(buffer, theme);
                render_tool_response(resp, debug);
            }
            MessageContent::ActionRequired(action) => {
                flush_markdown_buffer(buffer, theme);
                match &action.data {
                    ActionRequiredData::ToolConfirmation { tool_name, .. } => {
                        println!("action_required(tool_confirmation): {}", tool_name)
                    }
                    ActionRequiredData::Elicitation { message, .. } => {
                        println!("action_required(elicitation): {}", message)
                    }
                    ActionRequiredData::ElicitationResponse { id, .. } => {
                        println!("action_required(elicitation_response): {}", id)
                    }
                }
            }
            MessageContent::Image(image) => {
                flush_markdown_buffer(buffer, theme);
                println!("Image: [data: {}, type: {}]", image.data, image.mime_type);
            }
            MessageContent::Thinking(t) => {
                render_thinking_streaming(&t.thinking, buffer, thinking_header_shown, theme);
            }
            MessageContent::RedactedThinking(_) => {
                flush_markdown_buffer(buffer, theme);
                println!("\n{}", style("Thinking:").dim().italic());
                print_markdown("Thinking was redacted", theme);
            }
            MessageContent::SystemNotification(notification) => {
                match notification.notification_type {
                    SystemNotificationType::ThinkingMessage => {
                        show_thinking();
                        set_thinking_message(&notification.msg);
                    }
                    SystemNotificationType::InlineMessage => {
                        flush_markdown_buffer(buffer, theme);
                        hide_thinking();
                        println!("\n{}", style(&notification.msg).yellow());
                    }
                    SystemNotificationType::CreditsExhausted => {
                        flush_markdown_buffer(buffer, theme);
                        render_credits_exhausted_notification(notification);
                    }
                }
            }
            _ => {
                flush_markdown_buffer(buffer, theme);
                eprintln!("WARNING: Message content type could not be rendered");
            }
        }
    }

    let _ = std::io::stdout().flush();
}

fn render_credits_exhausted_notification(notification: &SystemNotificationContent) {
    hide_thinking();
    println!("\n{}", style(&notification.msg).yellow());

    if let Some(url) = notification
        .data
        .as_ref()
        .and_then(|d| d.get("top_up_url"))
        .and_then(|v| v.as_str())
    {
        println!(
            "{}",
            style(format!("Visit this URL to top up credits: {url}")).yellow()
        );
    }
}

pub fn get_credits_top_up_url(message: &Message) -> Option<String> {
    message.content.iter().find_map(|content| {
        let MessageContent::SystemNotification(notification) = content else {
            return None;
        };
        if notification.notification_type != SystemNotificationType::CreditsExhausted {
            return None;
        }
        notification
            .data
            .as_ref()
            .and_then(|d| d.get("top_up_url"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
}

pub fn flush_markdown_buffer(buffer: &mut MarkdownBuffer, theme: Theme) {
    let remaining = buffer.flush();
    if !remaining.is_empty() {
        print_markdown(&remaining, theme);
    }
}

pub fn flush_markdown_buffer_current_theme(buffer: &mut MarkdownBuffer) {
    flush_markdown_buffer(buffer, get_theme());
}

pub fn render_text(text: &str, color: Option<Color>, dim: bool) {
    render_text_no_newlines(format!("\n{}\n\n", text).as_str(), color, dim);
}

pub fn render_text_no_newlines(text: &str, color: Option<Color>, dim: bool) {
    if !std::io::stdout().is_terminal() {
        println!("{}", text);
        return;
    }
    let mut styled_text = style(text);
    if dim {
        styled_text = styled_text.dim();
    }
    if let Some(color) = color {
        styled_text = styled_text.fg(color);
    } else {
        styled_text = styled_text.green();
    }
    print!("{}", styled_text);
}

pub fn render_enter_plan_mode() {
    println!(
        "\n{} {}\n",
        style("Entering plan mode.").green().bold(),
        style("You can provide instructions to create a plan and then act on it. To exit early, type /endplan")
            .green()
            .dim()
    );
}

static LAST_TODO_RENDERED: Mutex<Option<String>> = Mutex::new(None);

pub fn render_act_on_plan() {
    println!(
        "\n{}\n",
        style("Exiting plan mode and acting on the above plan")
            .green()
            .bold(),
    );
}

pub fn render_exit_plan_mode() {
    println!("\n{}\n", style("Exiting plan mode.").green().bold());
}

pub fn goose_mode_message(text: &str) {
    println!("\n{}", style(text).yellow(),);
}

fn should_show_thinking() -> bool {
    Config::global()
        .get_param::<bool>("GOOSE_CLI_SHOW_THINKING")
        .unwrap_or(true)
        && std::io::stdout().is_terminal()
}

fn render_thinking(text: &str, theme: Theme) {
    if should_show_thinking() {
        println!("\n{}", style("Thinking:").dim().italic());
        print_markdown(text, theme);
    }
}

fn render_thinking_streaming(
    text: &str,
    buffer: &mut MarkdownBuffer,
    header_shown: &mut bool,
    theme: Theme,
) {
    if should_show_thinking() {
        flush_markdown_buffer(buffer, theme);
        if !*header_shown {
            println!("\n{}", style("Thinking:").dim().italic());
            *header_shown = true;
        }
        print!("{}", style(text).dim());
        let _ = std::io::stdout().flush();
    }
}

fn remember_tool_start(id: &str, summary: Option<String>) {
    let started_at = Instant::now();
    let summary = summary.unwrap_or_else(|| "tool".to_string());
    if let Ok(mut started) = tool_started().lock() {
        if started.len() >= MAX_TRACKED_TOOL_CALLS {
            started.clear();
        }
        started.insert(
            id.to_string(),
            RunningTool {
                started_at,
                summary: summary.clone(),
            },
        );
    }
    note_phase_tool_start(started_at, &summary);
}

fn take_tool_elapsed(id: &str) -> Option<Duration> {
    tool_started()
        .lock()
        .ok()
        .and_then(|mut started| started.remove(id).map(|tool| tool.started_at.elapsed()))
}

fn clear_running_tools() {
    if let Ok(mut started) = tool_started().lock() {
        started.clear();
    }
}

fn running_tool_summaries() -> Vec<String> {
    let Ok(started) = tool_started().lock() else {
        return Vec::new();
    };
    let mut summaries = started
        .values()
        .map(|tool| tool.summary.clone())
        .collect::<Vec<_>>();
    summaries.sort();
    summaries
}

fn print_tool_elapsed(elapsed: Option<Duration>, has_output: bool) {
    let Some(elapsed) = elapsed else {
        return;
    };
    if elapsed < LONG_TOOL_CALL_DURATION {
        return;
    }

    let elapsed = format!("{:.1}s", elapsed.as_secs_f64());
    if has_output {
        println!("    {}", style(elapsed).dim());
    } else {
        println!("  ⎿ {}", style(elapsed).dim());
    }
}

fn render_tool_request(req: &ToolRequest, theme: Theme, debug: bool) {
    match &req.tool_call {
        Ok(call) => {
            remember_tool_start(&req.id, Some(tool_request_status_summary(req, call)));
            if is_acp_tool_request(req) {
                return render_acp_request(req, call, debug);
            }

            match call.name.to_string().as_str() {
                name if is_shell_tool_name(name) => render_shell_request(call, debug),
                name if is_file_tool_name(name) => render_text_editor_request(call, debug),
                "execute_typescript" | "execute_code" => render_execute_code_request(call, debug),
                "delegate" => render_delegate_request(call, debug),
                "subagent" => render_delegate_request(call, debug),
                "todo__write" | "todo__todo_write" => render_todo_request(call, debug),
                "load" => {}
                _ => render_default_request(call, debug),
            }
        }
        Err(e) => {
            remember_tool_start(&req.id, None);
            print_markdown(&e.to_string(), theme);
        }
    }
}

fn render_tool_response(resp: &ToolResponse, debug: bool) {
    let config = Config::global();
    let elapsed = take_tool_elapsed(&resp.id);
    let mut has_output = false;

    match &resp.tool_result {
        Ok(result) => {
            for content in &result.content {
                if let Some(audience) = content.audience() {
                    if !audience.contains(&rmcp::model::Role::User) {
                        continue;
                    }
                }

                let min_priority = config
                    .get_param::<f32>("GOOSE_CLI_MIN_PRIORITY")
                    .ok()
                    .unwrap_or(DEFAULT_MIN_PRIORITY);

                if content
                    .priority()
                    .is_some_and(|priority| priority < min_priority)
                    || (content.priority().is_none() && !debug)
                {
                    continue;
                }

                if debug {
                    println!("{:#?}", content);
                    has_output = true;
                } else if let Some(text) = content.as_text() {
                    if !text.text.is_empty() {
                        has_output = true;
                    }
                    print_tool_output(&text.text);
                }
            }
        }
        Err(e) => {
            println!("    {}", style(e.to_string()).red().dim());
            has_output = true;
        }
    }
    print_tool_elapsed(elapsed, has_output);
}

fn is_acp_tool_request(req: &ToolRequest) -> bool {
    acp_tool_kind(req.tool_meta.as_ref()).is_some()
}

fn acp_tool_kind(tool_meta: Option<&Value>) -> Option<&str> {
    tool_meta
        .and_then(|meta| meta.get("goose.acp.kind"))
        .and_then(Value::as_str)
}

fn call_arguments_value(call: &CallToolRequestParams) -> Value {
    call.arguments
        .as_ref()
        .map(|arguments| Value::Object(arguments.clone()))
        .unwrap_or(Value::Null)
}

fn tool_request_status_summary(req: &ToolRequest, call: &CallToolRequestParams) -> String {
    let arguments = call_arguments_value(call);
    if let Some(summary) = acp_tool_kind(req.tool_meta.as_ref())
        .and_then(|kind| acp_call_summary(kind, req.tool_meta.as_ref(), &arguments))
    {
        return summary;
    }

    let name = call.name.to_string();
    let keys: &[&str] = if is_shell_tool_name(&name) {
        &["command", "cmd"]
    } else if is_file_tool_name(&name) {
        &["path", "file_path", "abs_path", "filePath"]
    } else {
        &[]
    };

    if let Some(summary) = acp_argument_string(&arguments, keys)
        .or_else(|| acp_first_string_argument(&arguments))
        .and_then(clean_acp_summary)
    {
        return summary;
    }

    let (tool, extension) = split_tool_name(&name);
    if extension.is_empty() {
        tool
    } else {
        format!("{tool} ({extension})")
    }
}

fn render_acp_request(req: &ToolRequest, call: &CallToolRequestParams, debug: bool) {
    let arguments = call_arguments_value(call);
    let summary = acp_tool_kind(req.tool_meta.as_ref())
        .and_then(|kind| acp_call_summary(kind, req.tool_meta.as_ref(), &arguments));
    let bullet = style("●").fg(active_or_default_color(Color::Cyan));
    let title = style(call.name.to_string()).bold();

    println!();
    println!("{} {}", bullet, title);
    if let Some(summary) = summary {
        println!("  ⎿ {}", style(summary).dim());
    }
    if debug {
        print_params(&call.arguments, 1, debug);
    }
}

fn acp_call_summary(kind: &str, tool_meta: Option<&Value>, arguments: &Value) -> Option<String> {
    if let Some(summary) = acp_location_summary(tool_meta) {
        return Some(summary);
    }

    let keys: &[&str] = match kind {
        "execute" => &["command", "cmd"],
        "read" | "edit" | "delete" | "move" => &["file_path", "path", "abs_path", "filePath"],
        "search" => &["pattern", "query"],
        "fetch" => &["url"],
        _ => &[],
    };

    acp_argument_string(arguments, keys)
        .or_else(|| acp_first_string_argument(arguments))
        .and_then(clean_acp_summary)
}

fn acp_location_summary(tool_meta: Option<&Value>) -> Option<String> {
    let locations = tool_meta?
        .get("goose.acp.locations")
        .and_then(Value::as_array)?;
    let first = locations.first()?;
    let path = first.get("path").and_then(Value::as_str)?;
    let mut summary = path.to_string();
    if let Some(line) = first.get("line").and_then(Value::as_u64) {
        summary.push(':');
        summary.push_str(&line.to_string());
    }
    if locations.len() > 1 {
        summary.push_str(&format!(" (+{} more)", locations.len() - 1));
    }
    clean_acp_summary(&summary)
}

fn acp_argument_string<'a>(arguments: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let object = arguments.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn acp_first_string_argument(arguments: &Value) -> Option<&str> {
    arguments.as_object()?.values().find_map(Value::as_str)
}

fn clean_acp_summary(raw: &str) -> Option<String> {
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cleaned.is_empty()).then(|| safe_truncate(&cleaned, 80))
}

fn print_tool_output(text: &str) {
    if text.is_empty() {
        return;
    }
    if !std::io::stdout().is_terminal() {
        print!("{}", text);
        return;
    }
    let max_lines = if get_show_full_tool_output() {
        usize::MAX
    } else {
        20
    };
    let lines: Vec<&str> = text.lines().collect();
    let is_diff = looks_like_diff(&lines);
    let prefix = |i: usize| if i == 0 { "  ⎿ " } else { "    " };
    let styled = |line: &str| -> String {
        let styled = if is_diff {
            if line.starts_with('+') {
                style(line).green()
            } else if line.starts_with('-') {
                style(line).red()
            } else if line.starts_with("@@") {
                style(line).cyan()
            } else {
                style(line).dim()
            }
        } else {
            style(line).dim()
        };
        styled.to_string()
    };
    if lines.len() <= max_lines {
        for (i, line) in lines.iter().enumerate() {
            println!("{}{}", prefix(i), styled(line));
        }
    } else {
        let head = max_lines / 2;
        let tail = max_lines - head;
        for (i, line) in lines[..head].iter().enumerate() {
            println!("{}{}", prefix(i), styled(line));
        }
        println!(
            "    {}",
            style(format!(
                "... ({} lines hidden, /toggle to show all)",
                lines.len() - head - tail
            ))
            .dim()
            .italic()
        );
        for line in &lines[lines.len() - tail..] {
            println!("    {}", styled(line));
        }
    }
}

/// Heuristic: color +/- lines only when the output actually looks like a diff,
/// so markdown bullets ("- item") don't light up red.
fn looks_like_diff(lines: &[&str]) -> bool {
    let has_marker = lines
        .iter()
        .any(|l| l.starts_with("@@") || l.starts_with("+++") || l.starts_with("---"));
    if has_marker {
        return true;
    }
    let plus = lines
        .iter()
        .any(|l| l.starts_with('+') && !l.starts_with("++"));
    let minus = lines
        .iter()
        .any(|l| l.starts_with('-') && !l.starts_with("--") && !l.starts_with("- "));
    plus && minus
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(name, "shell")
}

fn is_file_tool_name(name: &str) -> bool {
    matches!(name, "write" | "edit")
}

pub fn render_error(message: &str) {
    println!("\n  {} {}\n", style("error:").red().bold(), message);
}

pub fn render_prompts(prompts: &HashMap<String, Vec<String>>) {
    println!();
    for (extension, prompts) in prompts {
        println!(" {}", style(extension).green());
        for prompt in prompts {
            println!("  - {}", style(prompt).cyan());
        }
    }
    println!();
}

pub fn render_prompt_info(info: &PromptInfo) {
    println!();
    if let Some(ext) = &info.extension {
        println!(" {}: {}", style("Extension").green(), ext);
    }
    println!(" Prompt: {}", style(&info.name).cyan().bold());
    if let Some(desc) = &info.description {
        println!("\n {}", desc);
    }
    render_arguments(info);
    println!();
}

fn render_arguments(info: &PromptInfo) {
    if let Some(args) = &info.arguments {
        println!("\n Arguments:");
        for arg in args {
            let required = arg.required.unwrap_or(false);
            let req_str = if required {
                style("(required)").red()
            } else {
                style("(optional)").dim()
            };

            println!(
                "  {} {} {}",
                style(&arg.name).yellow(),
                req_str,
                arg.description.as_deref().unwrap_or("")
            );
        }
    }
}

pub fn render_extension_success(name: &str) {
    println!();
    println!(
        "  {} extension `{}`",
        style("added").green(),
        style(name).cyan(),
    );
    println!();
}

pub fn render_extension_error(name: &str, error: &str) {
    println!();
    println!(
        "  {} to add extension {}",
        style("failed").red(),
        style(name).red()
    );
    println!();
    println!("{}", style(error).dim());
    println!();
}

pub fn render_builtin_success(names: &str) {
    println!();
    println!(
        "  {} builtin{}: {}",
        style("added").green(),
        if names.contains(',') { "s" } else { "" },
        style(names).cyan()
    );
    println!();
}

pub fn render_builtin_error(names: &str, error: &str) {
    println!();
    println!(
        "  {} to add builtin{}: {}",
        style("failed").red(),
        if names.contains(',') { "s" } else { "" },
        style(names).red()
    );
    println!();
    println!("{}", style(error).dim());
    println!();
}

fn render_text_editor_request(call: &CallToolRequestParams, debug: bool) {
    print_tool_header(call);

    if let Some(args) = &call.arguments {
        if let Some(Value::String(path)) = args.get("path") {
            println!(
                "    {} {}",
                style("path").dim(),
                style(shorten_path(path, debug)).dim()
            );
        }

        let old_str = args.get("old_str").and_then(|v| v.as_str());
        let new_str = args.get("new_str").and_then(|v| v.as_str());
        let file_text = args.get("file_text").and_then(|v| v.as_str());
        if old_str.is_some() || new_str.is_some() || file_text.is_some() {
            print_edit_diff(old_str, new_str.or(file_text), debug);
        } else {
            let mut other_args = serde_json::Map::new();
            for (k, v) in args {
                if k != "path" {
                    other_args.insert(k.clone(), v.clone());
                }
            }
            if !other_args.is_empty() {
                print_params(&Some(other_args), 1, debug);
            }
        }
    }
    println!();
}

/// Render old/new edit content as a Claude-Code-style red/green mini diff.
fn print_edit_diff(old: Option<&str>, new: Option<&str>, debug: bool) {
    let cap = if debug || get_show_full_tool_output() {
        usize::MAX
    } else {
        12
    };
    for (content, sign) in [(old, '-'), (new, '+')] {
        let Some(content) = content else { continue };
        let total = content.lines().count();
        for (i, line) in content.lines().enumerate() {
            if i >= cap {
                let hidden = format!("{} … ({} more lines, /r to show all)", sign, total - cap);
                let hidden = style(hidden).dim().italic();
                println!("    {}", hidden);
                break;
            }
            let rendered = format!("{} {}", sign, line);
            let rendered = if sign == '-' {
                style(rendered).red()
            } else {
                style(rendered).green()
            };
            println!("    {}", rendered);
        }
    }
}

fn render_shell_request(call: &CallToolRequestParams, debug: bool) {
    print_tool_header(call);
    print_params(&call.arguments, 1, debug);
    println!();
}

fn render_execute_code_request(call: &CallToolRequestParams, debug: bool) {
    let tool_graph = call
        .arguments
        .as_ref()
        .and_then(|args| args.get("tool_graph"))
        .and_then(Value::as_array)
        .filter(|arr| !arr.is_empty());

    let Some(tool_graph) = tool_graph else {
        return render_default_request(call, debug);
    };

    let count = tool_graph.len();
    let plural = if count == 1 { "" } else { "s" };
    println!();
    println!(
        "  {} {} {} tool call{}",
        style("▸").dim(),
        style("execute").dim(),
        style(count).dim(),
        plural,
    );

    for (i, node) in tool_graph.iter().filter_map(Value::as_object).enumerate() {
        let tool = node
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let desc = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let deps: Vec<_> = node
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|d| (d + 1).to_string())
            .collect();
        let deps_str = if deps.is_empty() {
            String::new()
        } else {
            format!(" (uses {})", deps.join(", "))
        };
        println!(
            "    {}. {} {}{}",
            style(i + 1).dim(),
            style(tool).dim(),
            style(desc).dim(),
            style(deps_str).dim()
        );
    }

    let code = call
        .arguments
        .as_ref()
        .and_then(|args| args.get("code"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty());
    if code.is_some_and(|_| debug) {
        println!("{}", style(code.unwrap_or_default()).green());
    }

    println!();
}

fn render_delegate_request(call: &CallToolRequestParams, debug: bool) {
    print_tool_header(call);

    if let Some(args) = &call.arguments {
        if let Some(Value::String(source)) = args.get("source") {
            println!("    {} {}", style("source").dim(), style(source).dim());
        }

        if let Some(Value::String(instructions)) = args.get("instructions") {
            let display = if instructions.len() > 100 && !debug {
                safe_truncate(instructions, 100)
            } else {
                instructions.clone()
            };
            println!(
                "    {} {}",
                style("instructions").dim(),
                style(display).dim()
            );
        }

        if let Some(Value::Object(params)) = args.get("parameters") {
            println!("    {}:", style("parameters").dim());
            print_params(&Some(params.clone()), 2, debug);
        }

        let skip_keys = ["source", "instructions", "parameters"];
        let mut other_args = serde_json::Map::new();
        for (k, v) in args {
            if !skip_keys.contains(&k.as_str()) {
                other_args.insert(k.clone(), v.clone());
            }
        }
        if !other_args.is_empty() {
            print_params(&Some(other_args), 1, debug);
        }
    }

    println!();
}

#[derive(Debug, PartialEq, Eq)]
enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, PartialEq, Eq)]
enum TodoLine {
    Item {
        indent: String,
        status: TodoStatus,
        text: String,
    },
    Text(String),
}

fn parse_todo_line(line: &str) -> TodoLine {
    let first_non_whitespace = line
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(line.len());
    let (indent, rest) = line.split_at(first_non_whitespace);

    let Some(after_prefix) = rest.strip_prefix("- [") else {
        return TodoLine::Text(line.to_string());
    };
    let mut chars = after_prefix.chars();
    let Some(marker) = chars.next() else {
        return TodoLine::Text(line.to_string());
    };
    let after_marker = chars.as_str();
    let Some(text) = after_marker.strip_prefix("] ") else {
        return TodoLine::Text(line.to_string());
    };

    let status = match marker {
        ' ' => TodoStatus::Pending,
        '~' | '-' | '/' => TodoStatus::InProgress,
        'x' | 'X' => TodoStatus::Done,
        _ => return TodoLine::Text(line.to_string()),
    };

    TodoLine::Item {
        indent: indent.to_string(),
        status,
        text: text.to_string(),
    }
}

fn render_todo_checklist(content: &str) {
    const MAX_TODO_LINES: usize = 40;

    let lines: Vec<&str> = content.lines().collect();
    let hidden_lines = if !get_show_full_tool_output() && lines.len() > MAX_TODO_LINES {
        lines.len() - MAX_TODO_LINES
    } else {
        0
    };
    let visible_line_count = lines.len() - hidden_lines;

    for line in &lines[..visible_line_count] {
        match parse_todo_line(line) {
            TodoLine::Item {
                indent,
                status: TodoStatus::Pending,
                text,
            } => println!("    {}☐ {}", indent, text),
            TodoLine::Item {
                indent,
                status: TodoStatus::InProgress,
                text,
            } => println!(
                "    {} {}",
                style(format!("{indent}◐")).cyan(),
                style(text).cyan()
            ),
            TodoLine::Item {
                indent,
                status: TodoStatus::Done,
                text,
            } => println!(
                "    {} {}",
                style(format!("{indent}✔")).dim(),
                style(text).dim()
            ),
            TodoLine::Text(text) => println!("    {}", style(text).dim()),
        }
    }

    if hidden_lines > 0 {
        println!(
            "    {}",
            style(format!("… (+{} lines)", hidden_lines)).dim().italic()
        );
    }
}

fn render_todo_request(call: &CallToolRequestParams, debug: bool) {
    let Some(content) = call
        .arguments
        .as_ref()
        .and_then(|args| args.get("content"))
        .and_then(Value::as_str)
    else {
        render_default_request(call, debug);
        return;
    };

    print_tool_header(call);
    let mut last = LAST_TODO_RENDERED.lock().unwrap();
    if last.as_deref() == Some(content) {
        println!("    {}", style("(no changes)").dim());
    } else {
        render_todo_checklist(content);
        *last = Some(content.to_string());
    }
    println!();
}

fn render_default_request(call: &CallToolRequestParams, debug: bool) {
    print_tool_header(call);
    print_params(&call.arguments, 1, debug);
    println!();
}

fn split_tool_name(tool_name: &str) -> (String, String) {
    let parts: Vec<_> = tool_name.rsplit("__").collect();
    let tool = parts.first().copied().unwrap_or("unknown");
    let extension = parts
        .split_first()
        .map(|(_, s)| s.iter().rev().copied().collect::<Vec<_>>().join("__"))
        .unwrap_or_default();
    (tool.to_string(), extension_display_name(&extension))
}

fn extension_display_name(name: &str) -> String {
    match name {
        "code_execution" => "Code Mode".to_string(),
        _ => name.to_string(),
    }
}

pub fn format_subagent_tool_call_message(subagent_id: &str, tool_name: &str) -> String {
    let short_id = subagent_id.rsplit('_').next().unwrap_or(subagent_id);
    let (tool, extension) = split_tool_name(tool_name);

    if extension.is_empty() {
        format!("[subagent:{}] {}", short_id, tool)
    } else {
        format!("[subagent:{}] {} | {}", short_id, tool, extension)
    }
}

pub fn render_subagent_tool_call(
    subagent_id: &str,
    tool_name: &str,
    arguments: Option<&JsonObject>,
    debug: bool,
) {
    if tool_name == "code_execution__execute_typescript" {
        let tool_graph = arguments
            .and_then(|args| args.get("tool_graph"))
            .and_then(Value::as_array)
            .filter(|arr| !arr.is_empty());
        if let Some(tool_graph) = tool_graph {
            return render_subagent_tool_graph(subagent_id, tool_graph);
        }
    }
    let tool_header = format!(
        "  {} {}",
        style("▸").dim(),
        style(format_subagent_tool_call_message(subagent_id, tool_name)).dim(),
    );
    println!();
    println!("{}", tool_header);
    print_params(&arguments.cloned(), 1, debug);
    println!();
}

fn render_subagent_tool_graph(subagent_id: &str, tool_graph: &[Value]) {
    let short_id = subagent_id.rsplit('_').next().unwrap_or(subagent_id);
    let count = tool_graph.len();
    let plural = if count == 1 { "" } else { "s" };
    println!();
    println!(
        "  {} {} {} {} tool call{}",
        style("▸").dim(),
        style(format!("[subagent:{}]", short_id)).dim(),
        style("execute_typescript").dim(),
        style(count).dim(),
        plural,
    );

    for (i, node) in tool_graph.iter().filter_map(Value::as_object).enumerate() {
        let tool = node
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let desc = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let deps: Vec<_> = node
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|d| (d + 1).to_string())
            .collect();
        let deps_str = if deps.is_empty() {
            String::new()
        } else {
            format!(" (uses {})", deps.join(", "))
        };
        println!(
            "    {}. {} {}{}",
            style(i + 1).dim(),
            style(tool).dim(),
            style(desc).dim(),
            style(deps_str).dim()
        );
    }
    println!();
}

// Helper functions

fn print_tool_header(call: &CallToolRequestParams) {
    let (tool, extension) = split_tool_name(&call.name);
    let bullet = style("●").fg(active_or_default_color(Color::Cyan));
    let tool_header = if extension.is_empty() {
        format!("{} {}", bullet, style(&tool).bold())
    } else {
        format!(
            "{} {} {}",
            bullet,
            style(&tool).bold(),
            style(format!("({})", extension)).dim(),
        )
    };
    println!();
    println!("{}", tool_header);
}

// Respect NO_COLOR, as https://crates.io/crates/console already does
pub fn env_no_color() -> bool {
    // if NO_COLOR is defined at all disable colors
    std::env::var_os("NO_COLOR").is_none()
}

fn print_markdown(content: &str, theme: Theme) {
    if std::io::stdout().is_terminal() {
        if let Some((before, table, after)) = extract_markdown_table(content) {
            if !before.is_empty() {
                print_markdown_raw(&before, theme);
            }
            print_table(&table, theme);
            if !after.is_empty() {
                print_markdown(after, theme);
            }
        } else {
            print_markdown_raw(content, theme);
        }
    } else {
        print!("{}", content);
    }
}

/// Renders markdown content using bat (no table processing)
fn print_markdown_raw(content: &str, theme: Theme) {
    bat::PrettyPrinter::new()
        .input(bat::Input::from_bytes(content.as_bytes()))
        .theme(theme.as_str())
        .colored_output(env_no_color())
        .language("Markdown")
        .wrapping_mode(WrappingMode::NoWrapping(true))
        .print()
        .unwrap();
}

fn extract_markdown_table(content: &str) -> Option<(String, Vec<&str>, &str)> {
    let lines: Vec<&str> = content.lines().collect();

    // Track newline positions for safe slicing later
    let newline_indices: Vec<usize> = content
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
        .collect();

    // Skip tables inside code blocks
    let mut in_code_block = false;
    let mut table_start = None;
    let mut table_end = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            continue;
        }

        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            if table_start.is_none() {
                table_start = Some(i);
            }
            table_end = Some(i);
        } else if table_start.is_some() {
            break;
        }
    }

    let start = table_start?;
    let end = table_end?;

    // Need at least header + separator (2 rows minimum)
    if end < start + 1 {
        return None;
    }

    // Require separator to be the second row with proper format
    let separator_line = lines.get(start + 1)?;
    let is_valid_separator = separator_line.trim().starts_with('|')
        && separator_line.trim().ends_with('|')
        && separator_line
            .trim()
            .trim_matches('|')
            .split('|')
            .all(|cell| {
                let t = cell.trim();
                !t.is_empty() && t.chars().all(|c| c == '-' || c == ':' || c == ' ')
            });

    if !is_valid_separator {
        return None;
    }

    let before = lines[..start].join("\n");
    let before = if before.is_empty() {
        before
    } else {
        before + "\n"
    };
    let table = lines[start..=end].to_vec();

    let after = if end + 1 >= lines.len() {
        ""
    } else if let Some(&newline_pos) = newline_indices.get(end) {
        content.get(newline_pos + 1..).unwrap_or("")
    } else {
        ""
    };

    Some((before, table, after))
}

fn print_table(table_lines: &[&str], theme: Theme) {
    use comfy_table::{presets, Cell, CellAlignment, ContentArrangement, Table};

    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    table.load_preset(presets::ASCII_MARKDOWN);

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut alignments: Vec<CellAlignment> = Vec::new();
    let mut separator_idx = None;

    for (i, line) in table_lines.iter().enumerate() {
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|s| s.trim().to_string())
            .collect();

        let is_separator = cells.iter().all(|c| {
            let t = c.trim();
            t.chars().all(|ch| ch == '-' || ch == ':') && t.contains('-')
        });
        if is_separator {
            separator_idx = Some(i);
            alignments = cells
                .iter()
                .map(|c| {
                    let t = c.trim();
                    if t.starts_with(':') && t.ends_with(':') {
                        CellAlignment::Center
                    } else if t.ends_with(':') {
                        CellAlignment::Right
                    } else {
                        CellAlignment::Left
                    }
                })
                .collect();
        } else {
            rows.push(cells);
        }
    }

    if separator_idx.is_none() && !rows.is_empty() {
        alignments = vec![CellAlignment::Left; rows[0].len()];
    }

    if let Some(header) = rows.first() {
        let header_cells: Vec<Cell> = header
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let cell = Cell::new(text);
                if let Some(align) = alignments.get(i) {
                    cell.set_alignment(*align)
                } else {
                    cell
                }
            })
            .collect();
        table.set_header(header_cells);
    }

    for row in rows.iter().skip(1) {
        let cells: Vec<Cell> = row
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let cell = Cell::new(text);
                if let Some(align) = alignments.get(i) {
                    cell.set_alignment(*align)
                } else {
                    cell
                }
            })
            .collect();
        table.add_row(cells);
    }

    let table_str = table.to_string();
    print_markdown_raw(&table_str, theme);
}

const INDENT: &str = "    ";

fn print_value_with_prefix(prefix: &String, value: &Value, debug: bool) {
    let prefix_width = measure_text_width(prefix.as_str());
    print!("{}", prefix);
    print_value(value, debug, prefix_width)
}

fn print_value(value: &Value, debug: bool, reserve_width: usize) {
    let max_width = Term::stdout()
        .size_checked()
        .map(|(_h, w)| (w as usize).saturating_sub(reserve_width));
    let show_full = get_show_full_tool_output();
    let formatted = match value {
        Value::String(s) => match (max_width, debug || show_full) {
            (Some(w), false) if s.len() > w => style(safe_truncate(s, w)),
            _ => style(s.to_string()),
        }
        .green(),
        Value::Number(n) => style(n.to_string()).yellow(),
        Value::Bool(b) => style(b.to_string()).yellow(),
        Value::Null => style("null".to_string()).dim(),
        _ => unreachable!(),
    };
    println!("{}", formatted);
}

fn print_params(value: &Option<JsonObject>, depth: usize, debug: bool) {
    let indent = INDENT.repeat(depth);

    if let Some(json_object) = value {
        for (key, val) in json_object.iter() {
            match val {
                Value::Object(obj) => {
                    println!("{}{}:", indent, style(key).dim());
                    print_params(&Some(obj.clone()), depth + 1, debug);
                }
                Value::Array(arr) => {
                    // Check if all items are simple values (not objects or arrays)
                    let all_simple = arr.iter().all(|item| {
                        matches!(
                            item,
                            Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null
                        )
                    });

                    if all_simple {
                        // Render inline for simple arrays, truncation will be handled by print_value if needed
                        let values: Vec<String> = arr
                            .iter()
                            .map(|item| match item {
                                Value::String(s) => s.clone(),
                                Value::Number(n) => n.to_string(),
                                Value::Bool(b) => b.to_string(),
                                Value::Null => "null".to_string(),
                                _ => unreachable!(),
                            })
                            .collect();
                        let joined_values = values.join(", ");
                        print_value_with_prefix(
                            &format!("{}{}: ", indent, style(key).dim()),
                            &Value::String(joined_values),
                            debug,
                        );
                    } else {
                        // Use the original multi-line format for complex arrays
                        println!("{}{}:", indent, style(key).dim());
                        for item in arr.iter() {
                            if let Value::Object(obj) = item {
                                println!("{}{}- ", indent, INDENT);
                                print_params(&Some(obj.clone()), depth + 2, debug);
                            } else {
                                println!("{}{}- {}", indent, INDENT, item);
                            }
                        }
                    }
                }
                _ => {
                    print_value_with_prefix(
                        &format!("{}{}: ", indent, style(key).dim()),
                        val,
                        debug,
                    );
                }
            }
        }
    }
}

fn shorten_path(path: &str, debug: bool) -> String {
    // In debug mode, return the full path
    if debug {
        return path.to_string();
    }

    let path = Path::new(path);

    // First try to convert to ~ if it's in home directory
    let home = etcetera::home_dir().ok();
    let path_str = if let Some(home) = home {
        if let Ok(stripped) = path.strip_prefix(home) {
            format!("~/{}", stripped.display())
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };

    // If path is already short enough, return as is
    if path_str.len() <= 60 {
        return path_str;
    }

    let parts: Vec<_> = path_str.split('/').collect();

    // If we have 3 or fewer parts, return as is
    if parts.len() <= 3 {
        return path_str;
    }

    // Keep the first component (empty string before root / or ~) and last two components intact
    let mut shortened = vec![parts[0].to_string()];

    // Shorten middle components to their first letter
    for component in &parts[1..parts.len() - 2] {
        if !component.is_empty() {
            shortened.push(component.chars().next().unwrap_or('?').to_string());
        }
    }

    // Add the last two components
    shortened.push(parts[parts.len() - 2].to_string());
    shortened.push(parts[parts.len() - 1].to_string());

    shortened.join("/")
}

pub fn display_session_info(
    resume: bool,
    provider: &str,
    model: &str,
    session_id: &Option<String>,
) {
    set_terminal_title();

    let status = if resume {
        "resuming"
    } else if session_id.is_none() {
        "ephemeral"
    } else {
        "new session"
    };

    let model_display = model.to_string();

    let cwd_display = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // ASCII art goose with session info on the right
    println!();
    println!(
        "  {}  {} {} {} {} {}",
        style("  __( O)>").white(),
        style("●").green(),
        style(status).dim(),
        style("·").dim(),
        style(provider).dim(),
        style(&model_display).cyan(),
    );

    if let Some(id) = session_id {
        println!(
            "  {}  {} {} {}",
            style(r" \____)").white(),
            style(" ").dim(),
            style(id).dim(),
            style(format!("· {}", cwd_display)).dim(),
        );
    } else {
        println!(
            "  {}  {} {}",
            style(r" \____)").white(),
            style(" ").dim(),
            style(format!("  {}", cwd_display)).dim(),
        );
    }
    println!(
        "  {}  {}",
        style("   L L").white(),
        style("   goose is ready").white()
    );
}

fn set_terminal_title() {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let dir_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    // Sanitize: strip control characters (ESC, BEL, etc.) to prevent terminal escape injection
    let sanitized: String = dir_name.chars().filter(|c| !c.is_control()).collect();
    // OSC 0 sets the terminal window/tab title
    print!("\x1b]0;🪿 {}\x07", sanitized);
    let _ = std::io::stdout().flush();
}

pub fn display_context_usage(total_tokens: usize, context_limit: usize) {
    use console::style;

    if context_limit == 0 {
        println!(
            "  {}",
            style("context usage unavailable (context limit is 0)").dim()
        );
        return;
    }

    let percentage =
        (((total_tokens as f64 / context_limit as f64) * 100.0).round() as usize).min(100);

    let bar_width = 20;
    let filled = ((percentage as f64 / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width - filled.min(bar_width);

    let bar = format!("{}{}", "━".repeat(filled), "╌".repeat(empty));
    let colored_bar = if percentage < 50 {
        style(bar).green().dim()
    } else if percentage < 85 {
        style(bar).yellow()
    } else {
        style(bar).red()
    };

    fn format_tokens(n: usize) -> String {
        if n >= 1_000_000 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else if n >= 1_000 {
            format!("{:.0}k", n as f64 / 1_000.0)
        } else {
            n.to_string()
        }
    }

    println!(
        "  {} {} {}",
        colored_bar,
        style(format!("{}%", percentage)).dim(),
        style(format!(
            "{}/{}",
            format_tokens(total_tokens),
            format_tokens(context_limit)
        ))
        .dim(),
    );
}

fn estimate_cost_usd(provider: &str, model: &str, usage: &Usage) -> Option<f64> {
    let canonical_model = maybe_get_canonical_model(provider, model)?;
    canonical_model.cost.estimate_cost(usage)
}

/// Display cost information, if price data is available.
pub fn display_cost_usage(provider: &str, model: &str, usage: &Usage) {
    if let Some(cost) = estimate_cost_usd(provider, model, usage) {
        use console::style;
        let input_tokens = usage.input_tokens.unwrap_or(0);
        let output_tokens = usage.output_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let cache_write = usage.cache_write_input_tokens.unwrap_or(0);

        let cache_breakdown = match (cache_read, cache_write) {
            (0, 0) => String::new(),
            (read, 0) => format!(" ({} cache read)", read),
            (0, write) => format!(" ({} cache write)", write),
            (read, write) => format!(" ({} cache read, {} cache write)", read, write),
        };

        eprintln!(
            "Cost: {} USD ({} tokens: in {}{}, out {})",
            style(format!("${:.4}", cost)).cyan(),
            input_tokens + output_tokens,
            input_tokens,
            cache_breakdown,
            output_tokens
        );
    }
}

pub struct McpSpinners {
    bars: HashMap<String, ProgressBar>,
    log_spinner: Option<ProgressBar>,

    multi_bar: MultiProgress,
}

impl McpSpinners {
    pub fn new() -> Self {
        McpSpinners {
            bars: HashMap::new(),
            log_spinner: None,
            multi_bar: MultiProgress::new(),
        }
    }

    pub fn log(&mut self, message: &str) {
        let spinner = self.log_spinner.get_or_insert_with(|| {
            let bar = self.multi_bar.add(
                ProgressBar::new_spinner()
                    .with_style(
                        ProgressStyle::with_template("{spinner:.green} {msg}")
                            .unwrap()
                            .tick_chars("⠋⠙⠚⠛⠓⠒⠊⠉"),
                    )
                    .with_message(message.to_string()),
            );
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        });

        spinner.set_message(message.to_string());
    }

    pub fn update(&mut self, token: &str, value: f64, total: Option<f64>, message: Option<&str>) {
        let bar = self.bars.entry(token.to_string()).or_insert_with(|| {
            if let Some(total) = total {
                self.multi_bar.add(
                    ProgressBar::new((total * 100_f64) as u64).with_style(
                        ProgressStyle::with_template("[{elapsed}] {bar:40} {pos:>3}/{len:3} {msg}")
                            .unwrap(),
                    ),
                )
            } else {
                self.multi_bar.add(ProgressBar::new_spinner())
            }
        });
        bar.set_position((value * 100_f64) as u64);
        if let Some(msg) = message {
            bar.set_message(msg.to_string());
        }
    }

    pub fn hide(&mut self) -> Result<(), Error> {
        self.bars.iter_mut().for_each(|(_, bar)| {
            bar.disable_steady_tick();
        });
        if let Some(spinner) = self.log_spinner.as_mut() {
            spinner.disable_steady_tick();
        }
        self.multi_bar.clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::env;

    #[test]
    fn suppresses_status_only_while_output_is_recent() {
        let base = Instant::now();
        let window = Duration::from_millis(2000);
        // Output 500ms ago (within window) → suppress the spinner refresh.
        assert!(should_suppress_status(
            Some(base),
            base + Duration::from_millis(500),
            window
        ));
        // Output 3s ago (past window, e.g. a tool wait) → allow the spinner.
        assert!(!should_suppress_status(
            Some(base),
            base + Duration::from_millis(3000),
            window
        ));
        // No output yet (initial wait) → allow the spinner.
        assert!(!should_suppress_status(
            None,
            base + Duration::from_millis(500),
            window
        ));
    }

    #[test]
    fn test_short_paths_unchanged() {
        assert_eq!(shorten_path("/usr/bin", false), "/usr/bin");
        assert_eq!(shorten_path("/a/b/c", false), "/a/b/c");
        assert_eq!(shorten_path("file.txt", false), "file.txt");
    }

    #[test]
    fn test_debug_mode_returns_full_path() {
        assert_eq!(
            shorten_path("/very/long/path/that/would/normally/be/shortened", true),
            "/very/long/path/that/would/normally/be/shortened"
        );
    }

    #[test]
    fn test_home_directory_conversion() {
        // Save the current home dir
        let original_home = env::var("HOME").ok();

        // Set a test home directory
        env::set_var("HOME", "/Users/testuser");

        assert_eq!(
            shorten_path("/Users/testuser/documents/file.txt", false),
            "~/documents/file.txt"
        );

        // A path that starts similarly to home but isn't in home
        assert_eq!(
            shorten_path("/Users/testuser2/documents/file.txt", false),
            "/Users/testuser2/documents/file.txt"
        );

        // Restore the original home dir
        if let Some(home) = original_home {
            env::set_var("HOME", home);
        } else {
            env::remove_var("HOME");
        }
    }

    #[test]
    fn test_toggle_full_tool_output() {
        let initial = get_show_full_tool_output();

        let after_first_toggle = toggle_full_tool_output();
        assert_eq!(after_first_toggle, !initial);
        assert_eq!(get_show_full_tool_output(), after_first_toggle);

        let after_second_toggle = toggle_full_tool_output();
        assert_eq!(after_second_toggle, initial);
        assert_eq!(get_show_full_tool_output(), initial);
    }

    #[test]
    fn thinking_status_label_formats_elapsed_seconds_and_minutes() {
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "Thinking...",
            elapsed: Duration::from_secs(59),
            role: None,
            running_tools: &[],
            terminal_width: None,
            hint: None,
        });
        assert_eq!(label, "Thinking... for 59s");

        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "Thinking...",
            elapsed: Duration::from_secs(79),
            role: None,
            running_tools: &[],
            terminal_width: None,
            hint: None,
        });
        assert_eq!(label, "Thinking... for 1m 19s");
    }

    #[test]
    fn thinking_status_label_describes_zero_one_and_many_tools() {
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "model working...",
            elapsed: Duration::from_secs(7),
            role: None,
            running_tools: &[],
            terminal_width: None,
            hint: None,
        });
        assert_eq!(label, "model working... for 7s");

        let running_tools = vec!["cargo test -p goose-cli".to_string()];
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "model working...",
            elapsed: Duration::from_secs(7),
            role: None,
            running_tools: &running_tools,
            terminal_width: None,
            hint: None,
        });
        assert_eq!(
            label,
            "model working... for 7s · cargo test -p goose-cli running"
        );

        let running_tools = vec!["read src/main.rs".to_string(), "cargo test".to_string()];
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "model working...",
            elapsed: Duration::from_secs(7),
            role: None,
            running_tools: &running_tools,
            terminal_width: None,
            hint: None,
        });
        assert_eq!(label, "model working... for 7s · 2 tools running");
    }

    #[test]
    fn thinking_status_label_includes_orch_role_and_cycle() {
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "codex/gpt-5 working...",
            elapsed: Duration::from_secs(61),
            role: Some(ActiveRoleStatus {
                role: ActiveRole::Implementer,
                cycle: Some((2, 3)),
            }),
            running_tools: &[],
            terminal_width: None,
            hint: None,
        });
        assert_eq!(label, "implementer c2/3 · codex/gpt-5 working... for 1m 1s");
    }

    #[test]
    fn thinking_status_label_truncates_to_display_width() {
        let label = build_thinking_status_label(ThinkingStatusLabelInput {
            base: "very-long-model-name working...",
            elapsed: Duration::from_secs(7),
            role: Some(ActiveRoleStatus {
                role: ActiveRole::Planner,
                cycle: None,
            }),
            running_tools: &["cargo test -p goose-cli".to_string()],
            terminal_width: Some(24),
            hint: None,
        });

        assert_eq!(measure_text_width(&label), 24);
        assert_eq!(label, "planner · very-long-m...");
    }

    #[test]
    fn phase_progress_formats_cycle_tools_and_last_summary() {
        let label = format_phase_progress(PhaseProgressInput {
            label: "implement",
            cycle: Some((1, 3)),
            elapsed: Duration::from_secs(372),
            tool_calls: 14,
            last_summary: Some("edit runner.rs"),
            terminal_width: None,
        });

        assert_eq!(
            label,
            "implement c1/3 · 6m 12s · 14 tool calls · last: edit runner.rs"
        );
    }

    #[test]
    fn phase_progress_formats_single_tool_without_plural_suffix() {
        let label = format_phase_progress(PhaseProgressInput {
            label: "implement",
            cycle: Some((1, 3)),
            elapsed: Duration::from_secs(1),
            tool_calls: 1,
            last_summary: Some("cargo test -p goose-cli"),
            terminal_width: None,
        });

        assert_eq!(
            label,
            "implement c1/3 · 1s · 1 tool call · last: cargo test -p goose-cli"
        );
    }

    #[test]
    fn phase_progress_describes_text_only_phase_as_working() {
        let label = format_phase_progress(PhaseProgressInput {
            label: "review",
            cycle: None,
            elapsed: Duration::from_secs(123),
            tool_calls: 0,
            last_summary: None,
            terminal_width: None,
        });

        assert_eq!(label, "review · 2m 3s · working…");
    }

    #[test]
    fn phase_progress_truncates_to_display_width() {
        let label = format_phase_progress(PhaseProgressInput {
            label: "implement",
            cycle: Some((1, 3)),
            elapsed: Duration::from_secs(372),
            tool_calls: 14,
            last_summary: Some("a very long command that should not fill the terminal"),
            terminal_width: Some(36),
        });

        assert_eq!(measure_text_width(&label), 36);
        assert!(label.ends_with("..."));
    }

    #[test]
    fn thinking_indicator_fresh_turn_resets_stale_start_after_unguarded_hide() {
        let mut indicator = ThinkingIndicator::default();
        indicator.begin_turn();
        let stale_start = indicator.turn_started_at.unwrap();
        indicator.hide();

        indicator.begin_fresh_turn();

        assert!(indicator.turn_started_at.unwrap() > stale_start);
    }

    #[test]
    fn thinking_indicator_dynamic_message_overrides_static_context() {
        set_thinking_context(Some("provider/model working...".to_string()));
        let mut indicator = ThinkingIndicator::default();
        indicator.begin_turn();

        indicator.set_base_message("Compacting context...".to_string());

        assert_eq!(indicator.current_base_message(), "Compacting context...");
        set_thinking_context(None);
    }

    #[test]
    fn test_long_path_shortening() {
        assert_eq!(
            shorten_path(
                "/vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv/long/path/with/many/components/file.txt",
                false
            ),
            "/v/l/p/w/m/components/file.txt"
        );
    }

    #[test]
    fn test_get_credits_top_up_url_from_credits_notification() {
        let message = Message::assistant().with_system_notification_with_data(
            SystemNotificationType::CreditsExhausted,
            "Insufficient credits",
            json!({"top_up_url": "https://router.tetrate.ai/billing"}),
        );
        assert_eq!(
            get_credits_top_up_url(&message).as_deref(),
            Some("https://router.tetrate.ai/billing")
        );
    }

    #[test]
    fn test_get_credits_top_up_url_ignores_non_credits_notification() {
        let message = Message::assistant().with_system_notification_with_data(
            SystemNotificationType::InlineMessage,
            "hello",
            json!({"top_up_url": "https://router.tetrate.ai/billing"}),
        );
        assert_eq!(get_credits_top_up_url(&message), None);
    }

    #[test]
    fn test_parse_todo_line_recognizes_status_markers() {
        assert_eq!(
            parse_todo_line("- [ ] Pending task"),
            TodoLine::Item {
                indent: String::new(),
                status: TodoStatus::Pending,
                text: "Pending task".to_string(),
            }
        );
        assert_eq!(
            parse_todo_line("- [~] Task in progress"),
            TodoLine::Item {
                indent: String::new(),
                status: TodoStatus::InProgress,
                text: "Task in progress".to_string(),
            }
        );
        assert_eq!(
            parse_todo_line("- [x] Completed task"),
            TodoLine::Item {
                indent: String::new(),
                status: TodoStatus::Done,
                text: "Completed task".to_string(),
            }
        );
        assert_eq!(
            parse_todo_line("- [X] Completed task"),
            TodoLine::Item {
                indent: String::new(),
                status: TodoStatus::Done,
                text: "Completed task".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_todo_line_preserves_indent_and_falls_back() {
        assert_eq!(
            parse_todo_line("  - [ ] Sub-task"),
            TodoLine::Item {
                indent: "  ".to_string(),
                status: TodoStatus::Pending,
                text: "Sub-task".to_string(),
            }
        );
        assert_eq!(
            parse_todo_line("Notes"),
            TodoLine::Text("Notes".to_string())
        );
        assert_eq!(
            parse_todo_line("- [?] Mystery"),
            TodoLine::Text("- [?] Mystery".to_string())
        );
    }

    #[test]
    fn acp_call_summary_prefers_locations_with_line_numbers() {
        let tool_meta = json!({
            "goose.acp.locations": [
                {"path": "crates/goose-cli/src/session/output.rs", "line": 42},
                {"path": "crates/goose-cli/src/session/mod.rs"}
            ]
        });
        let arguments = json!({"command": "cargo test"});

        assert_eq!(
            acp_call_summary("execute", Some(&tool_meta), &arguments).as_deref(),
            Some("crates/goose-cli/src/session/output.rs:42 (+1 more)")
        );
    }

    #[test]
    fn acp_call_summary_falls_back_to_kind_specific_arguments() {
        assert_eq!(
            acp_call_summary(
                "execute",
                None,
                &json!({"command": "cargo clippy --all-targets -- -D warnings\n"})
            )
            .as_deref(),
            Some("cargo clippy --all-targets -- -D warnings")
        );

        assert_eq!(
            acp_call_summary("edit", None, &json!({"file_path": "src/main.rs"})).as_deref(),
            Some("src/main.rs")
        );
    }
}
