use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const ITERM2_KEY: &str = "0xd-0x20000-0x24";
const ITERM2_VALUE: &str =
    r#"{ Action = 11; Text = "0x1b 0x0d"; Version = 1; Keycode = 13; Modifiers = 131072; }"#;
const VSCODE_BINDING: &str = r#"{
    "key": "shift+enter",
    "command": "workbench.action.terminal.sendSequence",
    "when": "terminalFocus",
    "args": { "text": "\u001b\r" }
  }"#;
const KITTY_LINE: &str = r#"map shift+enter send_text all \x1b\r"#;
const KITTY_BLOCK: &str = r#"# >>> goose terminal setup: Shift+Enter newline
map shift+enter send_text all \x1b\r
# <<< goose terminal setup
"#;
const ALACRITTY_BLOCK: &str = r#"# >>> goose terminal setup: Shift+Enter newline
[[keyboard.bindings]]
chars = "\u001B\r"
key = "Return"
mods = "Shift"
# <<< goose terminal setup
"#;
const WEZTERM_BLOCK: &str = r#"-- >>> goose terminal setup: Shift+Enter newline
config.keys = config.keys or {}
table.insert(config.keys, {
  key = 'Enter',
  mods = 'SHIFT',
  action = wezterm.action.SendString('\x1b\r'),
})
-- <<< goose terminal setup
"#;
const WEZTERM_NEW_CONFIG: &str = r#"local wezterm = require 'wezterm'
local config = {}
config.keys = {
  { key = 'Enter', mods = 'SHIFT', action = wezterm.action.SendString('\x1b\r') },
}
return config
"#;
const GOOSE_MARKER: &str = "goose terminal setup: Shift+Enter newline";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NewlineHintState {
    pub terminal: DetectedTerminal,
    pub shift_enter_configured: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct TerminalEnv {
    vars: HashMap<String, String>,
}

impl TerminalEnv {
    pub(super) fn from_current() -> Self {
        Self {
            vars: env::vars().collect(),
        }
    }

    #[cfg(test)]
    fn from_pairs(vars: &[(&str, &str)]) -> Self {
        Self {
            vars: vars
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    fn display_value(&self, key: &str) -> &str {
        self.get(key).unwrap_or("<unset>")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetectedTerminal {
    Iterm2,
    VsCode,
    Kitty,
    WezTerm,
    Alacritty,
    AppleTerminal,
    Unsupported,
}

impl DetectedTerminal {
    fn name(self) -> &'static str {
        match self {
            Self::Iterm2 => "iTerm2",
            Self::VsCode => "VS Code terminal",
            Self::Kitty => "kitty",
            Self::WezTerm => "WezTerm",
            Self::Alacritty => "Alacritty",
            Self::AppleTerminal => "Apple Terminal",
            Self::Unsupported => "unsupported terminal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OsFamily {
    Mac,
    Linux,
    Windows,
}

impl OsFamily {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InstallStatusKind {
    Installed,
    AlreadyInstalled,
}

#[derive(Clone, Debug)]
pub(super) struct InstallStatus {
    pub status: InstallStatusKind,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    fn shell_preview(&self) -> String {
        let args = self
            .args
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {}", self.program, args)
    }
}

pub(super) fn detect_terminal(env: &TerminalEnv) -> DetectedTerminal {
    let term_program = env.get("TERM_PROGRAM").unwrap_or("").to_ascii_lowercase();
    let term = env.get("TERM").unwrap_or("").to_ascii_lowercase();

    if term_program.contains("iterm") {
        DetectedTerminal::Iterm2
    } else if term_program.contains("vscode") || term_program.contains("cursor") {
        DetectedTerminal::VsCode
    } else if term_program.contains("apple_terminal") {
        DetectedTerminal::AppleTerminal
    } else if term_program.contains("wezterm") || env.get("WEZTERM_EXECUTABLE").is_some() {
        DetectedTerminal::WezTerm
    } else if term_program.contains("alacritty") || term.contains("alacritty") {
        DetectedTerminal::Alacritty
    } else if term == "xterm-kitty" || env.get("KITTY_PID").is_some() {
        DetectedTerminal::Kitty
    } else {
        DetectedTerminal::Unsupported
    }
}

pub(super) fn run_terminal_setup() -> Result<()> {
    let env = TerminalEnv::from_current();
    let terminal = detect_terminal(&env);
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        bail!("Could not locate $HOME; cannot install terminal key bindings");
    };
    let os = OsFamily::current();

    println!("Detected terminal: {}", terminal.name());

    match terminal {
        DetectedTerminal::Iterm2 => run_iterm2_setup(),
        DetectedTerminal::VsCode => run_file_setup(
            terminal,
            vscode_keybindings_path(&home, os),
            VSCODE_BINDING,
            || install_vscode_keybinding(&home, os),
        ),
        DetectedTerminal::Kitty => run_file_setup(
            terminal,
            kitty_config_path(&home),
            KITTY_BLOCK.trim_end(),
            || install_kitty_keybinding(&home),
        ),
        DetectedTerminal::WezTerm => run_file_setup(
            terminal,
            wezterm_config_path(&home),
            WEZTERM_BLOCK.trim_end(),
            || install_wezterm_keybinding(&home),
        ),
        DetectedTerminal::Alacritty => run_file_setup(
            terminal,
            alacritty_config_path(&home),
            ALACRITTY_BLOCK.trim_end(),
            || install_alacritty_keybinding(&home),
        ),
        DetectedTerminal::AppleTerminal | DetectedTerminal::Unsupported => {
            println!("{}", unsupported_message(terminal, &env));
            Ok(())
        }
    }
}

pub(super) fn default_newline_hint_state() -> NewlineHintState {
    NewlineHintState {
        terminal: DetectedTerminal::Unsupported,
        shift_enter_configured: false,
    }
}

pub(super) fn newline_hint_state_from_current_env() -> NewlineHintState {
    let env = TerminalEnv::from_current();
    let terminal = detect_terminal(&env);
    let shift_enter_configured = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| shift_enter_configured(terminal, &home, OsFamily::current()))
        .unwrap_or(false);

    NewlineHintState {
        terminal,
        shift_enter_configured,
    }
}

fn run_iterm2_setup() -> Result<()> {
    if iterm2_binding_installed() {
        println!("Already installed: iTerm2 Shift+Enter sends ESC then carriage return.");
        return Ok(());
    }

    let command = iterm2_defaults_command();
    println!("Goose will configure Shift+Enter to send ESC then carriage return.");
    println!("Target: iTerm2 preferences domain com.googlecode.iterm2, GlobalKeyMap[{ITERM2_KEY}]");
    println!("Command:\n  {}", command.shell_preview());

    if !confirm("Install this iTerm2 key binding?")? {
        println!("Cancelled. No changes made.");
        return Ok(());
    }

    let output = Command::new(&command.program)
        .args(&command.args)
        .output()
        .context("failed to run defaults for iTerm2 key binding")?;

    if output.status.success() {
        println!("Installed: iTerm2 Shift+Enter now sends ESC then carriage return.");
        println!("Restart iTerm2 or open a new window if the change is not picked up immediately.");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to install iTerm2 key binding: {}", stderr.trim());
    }
}

fn run_file_setup<F>(
    terminal: DetectedTerminal,
    target: PathBuf,
    snippet: &str,
    install: F,
) -> Result<()>
where
    F: FnOnce() -> Result<InstallStatus>,
{
    if target
        .exists()
        .then(|| fs::read_to_string(&target).ok())
        .flatten()
        .as_deref()
        .is_some_and(config_has_binding)
    {
        println!(
            "Already installed: {} Shift+Enter binding exists in {}.",
            terminal.name(),
            target.display()
        );
        return Ok(());
    }

    println!("Goose will configure Shift+Enter to send ESC then carriage return.");
    println!("Target file: {}", target.display());
    println!("Content to add:\n{snippet}");

    if !confirm(&format!("Update {}?", target.display()))? {
        println!("Cancelled. No changes made.");
        return Ok(());
    }

    match install() {
        Ok(status) => {
            match status.status {
                InstallStatusKind::Installed => println!("Installed: {}", status.message),
                InstallStatusKind::AlreadyInstalled => {
                    println!("Already installed: {}", status.message)
                }
            }
            println!("Target: {}", status.target);
            Ok(())
        }
        Err(err) => {
            println!(
                "Failed to install {} Shift+Enter binding: {err}",
                terminal.name()
            );
            Err(err)
        }
    }
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;

    if !io::stdin().is_terminal() {
        println!();
        return Ok(false);
    }

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

pub(super) fn newline_control_hint(
    terminal: DetectedTerminal,
    shift_enter_configured: bool,
    newline_key: char,
) -> String {
    let newline_key = newline_key.to_ascii_uppercase();
    match terminal {
        DetectedTerminal::AppleTerminal => {
            format!("Enter to send · Option+Enter newline · Ctrl+{newline_key} newline")
        }
        DetectedTerminal::Unsupported => {
            format!("Enter to send · Option+Enter newline · Ctrl+{newline_key} newline")
        }
        _ if shift_enter_configured => {
            format!("Enter to send · Shift+Enter newline · Ctrl+{newline_key} newline")
        }
        _ => format!(
            "Enter to send · if Shift+Enter submits, run /terminal-setup (Option+Enter works now) · Ctrl+{newline_key} newline"
        ),
    }
}

fn shift_enter_configured(terminal: DetectedTerminal, home: &Path, os: OsFamily) -> bool {
    match terminal {
        DetectedTerminal::Iterm2 => iterm2_binding_installed(),
        DetectedTerminal::VsCode => file_has_binding(&vscode_keybindings_path(home, os)),
        DetectedTerminal::Kitty => file_has_binding(&kitty_config_path(home)),
        DetectedTerminal::WezTerm => file_has_binding(&wezterm_config_path(home)),
        DetectedTerminal::Alacritty => file_has_binding(&alacritty_config_path(home)),
        DetectedTerminal::AppleTerminal | DetectedTerminal::Unsupported => false,
    }
}

fn file_has_binding(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| config_has_binding(&content))
        .unwrap_or(false)
}

fn config_has_binding(content: &str) -> bool {
    content.contains(GOOSE_MARKER)
        || content.contains(KITTY_LINE)
        || (content.contains("workbench.action.terminal.sendSequence")
            && content.to_ascii_lowercase().contains("shift+enter")
            && (content.contains(r#"\u001b\r"#) || content.contains(r#"\u001B\r"#)))
        || (content.contains("[[keyboard.bindings]]")
            && content.contains(r#"chars = "\u001B\r""#)
            && content.contains(r#"mods = "Shift""#))
        || (content.contains("wezterm.action.SendString")
            && content.contains("SHIFT")
            && (content.contains(r#"\x1b\r"#) || content.contains(r#"\u{1b}\r"#)))
}

fn unsupported_message(terminal: DetectedTerminal, env: &TerminalEnv) -> String {
    match terminal {
        DetectedTerminal::AppleTerminal => {
            "Apple Terminal does not expose a Shift+Enter key binding that can send a distinguishable sequence to goose. No changes were made. Use Option+Enter for a newline, or Ctrl+J if Option is not configured as Meta.".to_string()
        }
        _ => format!(
            "Unsupported terminal for /terminal-setup. No changes were made. TERM_PROGRAM={}, TERM={}. Use Option+Enter or Ctrl+J for newlines.",
            env.display_value("TERM_PROGRAM"),
            env.display_value("TERM")
        ),
    }
}

fn iterm2_defaults_command() -> CommandSpec {
    CommandSpec {
        program: "/usr/bin/defaults".to_string(),
        args: vec![
            "write".to_string(),
            "com.googlecode.iterm2".to_string(),
            "GlobalKeyMap".to_string(),
            "-dict-add".to_string(),
            ITERM2_KEY.to_string(),
            ITERM2_VALUE.to_string(),
        ],
    }
}

fn iterm2_binding_installed() -> bool {
    let output = Command::new("/usr/bin/defaults")
        .args(["read", "com.googlecode.iterm2", "GlobalKeyMap", ITERM2_KEY])
        .output();

    output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .is_some_and(|stdout| {
            let stdout = stdout.to_ascii_lowercase();
            stdout.contains("0x1b 0x0d") || (stdout.contains("0x1b") && stdout.contains("0x0d"))
        })
}

fn install_vscode_keybinding(home: &Path, os: OsFamily) -> Result<InstallStatus> {
    let path = vscode_keybindings_path(home, os);
    let content = fs::read_to_string(&path).unwrap_or_default();

    if config_has_binding(&content) {
        return Ok(already_installed(
            path,
            "VS Code keybinding already sends ESC+Enter.",
        ));
    }

    let updated = append_json_array_item(&content, VSCODE_BINDING)?;
    write_file(&path, updated)?;
    Ok(installed(
        path,
        "VS Code terminal Shift+Enter keybinding installed. Reload the window or open a new terminal if needed.",
    ))
}

fn install_kitty_keybinding(home: &Path) -> Result<InstallStatus> {
    append_block_once(
        kitty_config_path(home),
        KITTY_BLOCK,
        "kitty Shift+Enter keybinding installed. kitty normally reloads config automatically; otherwise press Ctrl+Shift+F5.",
    )
}

fn install_alacritty_keybinding(home: &Path) -> Result<InstallStatus> {
    append_block_once(
        alacritty_config_path(home),
        ALACRITTY_BLOCK,
        "Alacritty Shift+Enter keybinding installed. Restart Alacritty for the change to apply.",
    )
}

fn install_wezterm_keybinding(home: &Path) -> Result<InstallStatus> {
    let path = wezterm_config_path(home);
    let content = fs::read_to_string(&path).unwrap_or_default();

    if config_has_binding(&content) {
        return Ok(already_installed(
            path,
            "WezTerm keybinding already sends ESC+Enter.",
        ));
    }

    if content.is_empty() {
        write_file(&path, WEZTERM_NEW_CONFIG.to_string())?;
        return Ok(installed(
            path,
            "WezTerm Shift+Enter keybinding installed. Restart WezTerm or reload config if needed.",
        ));
    }

    let Some(idx) = content.find("return config") else {
        bail!(
            "could not safely update WezTerm config; expected a `return config` line. Add this block manually before your return statement:\n{}",
            WEZTERM_BLOCK.trim_end()
        );
    };

    let (before_return, return_stmt) = content.split_at(idx);
    let mut updated = String::new();
    updated.push_str(before_return.trim_end());
    updated.push_str("\n\n");
    updated.push_str(WEZTERM_BLOCK);
    updated.push('\n');
    updated.push_str(return_stmt);

    write_file(&path, updated)?;
    Ok(installed(
        path,
        "WezTerm Shift+Enter keybinding installed. Restart WezTerm or reload config if needed.",
    ))
}

fn append_block_once(path: PathBuf, block: &str, message: &str) -> Result<InstallStatus> {
    let content = fs::read_to_string(&path).unwrap_or_default();

    if config_has_binding(&content) {
        return Ok(already_installed(path, message));
    }

    let mut updated = content.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(block);
    if !updated.ends_with('\n') {
        updated.push('\n');
    }

    write_file(&path, updated)?;
    Ok(installed(path, message))
}

fn append_json_array_item(content: &str, item: &str) -> Result<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(format!("[\n  {item}\n]\n"));
    }

    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        bail!("expected a JSON array in VS Code keybindings.json");
    }

    let close = content
        .rfind(']')
        .ok_or_else(|| anyhow!("expected a JSON array in VS Code keybindings.json"))?;
    let (before_close, _) = content.split_at(close);
    let before_close = before_close.trim_end();
    let body = before_close
        .trim_start()
        .strip_prefix('[')
        .unwrap_or(before_close)
        .trim();
    let comma = if body.is_empty() { "" } else { "," };

    Ok(format!("{before_close}{comma}\n  {item}\n]\n"))
}

fn write_file(path: &Path, content: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn installed(path: PathBuf, message: &str) -> InstallStatus {
    InstallStatus {
        status: InstallStatusKind::Installed,
        target: path.display().to_string(),
        message: message.to_string(),
    }
}

fn already_installed(path: PathBuf, message: &str) -> InstallStatus {
    InstallStatus {
        status: InstallStatusKind::AlreadyInstalled,
        target: path.display().to_string(),
        message: message.to_string(),
    }
}

fn vscode_keybindings_path(home: &Path, os: OsFamily) -> PathBuf {
    match os {
        OsFamily::Mac => home.join("Library/Application Support/Code/User/keybindings.json"),
        OsFamily::Linux => home.join(".config/Code/User/keybindings.json"),
        OsFamily::Windows => home.join("AppData/Roaming/Code/User/keybindings.json"),
    }
}

fn kitty_config_path(home: &Path) -> PathBuf {
    home.join(".config/kitty/kitty.conf")
}

fn alacritty_config_path(home: &Path) -> PathBuf {
    home.join(".config/alacritty/alacritty.toml")
}

fn wezterm_config_path(home: &Path) -> PathBuf {
    home.join(".wezterm.lua")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

impl fmt::Display for InstallStatusKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Installed => f.write_str("installed"),
            Self::AlreadyInstalled => f.write_str("already installed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env(vars: &[(&str, &str)]) -> TerminalEnv {
        TerminalEnv::from_pairs(vars)
    }

    #[test]
    fn detects_supported_terminals_from_environment() {
        assert_eq!(
            detect_terminal(&env(&[("TERM_PROGRAM", "iTerm.app")])),
            DetectedTerminal::Iterm2
        );
        assert_eq!(
            detect_terminal(&env(&[("TERM_PROGRAM", "vscode")])),
            DetectedTerminal::VsCode
        );
        assert_eq!(
            detect_terminal(&env(&[("TERM", "xterm-kitty")])),
            DetectedTerminal::Kitty
        );
        assert_eq!(
            detect_terminal(&env(&[(
                "WEZTERM_EXECUTABLE",
                "/Applications/WezTerm.app/wezterm"
            )])),
            DetectedTerminal::WezTerm
        );
        assert_eq!(
            detect_terminal(&env(&[("TERM", "alacritty")])),
            DetectedTerminal::Alacritty
        );
    }

    #[test]
    fn apple_terminal_is_detected_as_unsupported_with_option_enter_hint() {
        let message = unsupported_message(
            DetectedTerminal::AppleTerminal,
            &env(&[("TERM_PROGRAM", "Apple_Terminal")]),
        );

        assert!(message.contains("Apple Terminal"));
        assert!(message.contains("Option+Enter"));
        assert!(message.contains("Ctrl+J"));
    }

    #[test]
    fn unsupported_terminal_message_includes_detected_environment() {
        let message = unsupported_message(
            DetectedTerminal::Unsupported,
            &env(&[("TERM_PROGRAM", "MysteryTerm"), ("TERM", "xterm-256color")]),
        );

        assert!(message.contains("MysteryTerm"));
        assert!(message.contains("xterm-256color"));
        assert!(message.contains("/terminal-setup"));
    }

    #[test]
    fn newline_hint_suggests_terminal_setup_when_keymap_is_missing() {
        let hint = newline_control_hint(DetectedTerminal::VsCode, false, 'j');

        assert!(hint.contains("/terminal-setup"));
        assert!(hint.contains("Option+Enter"));
        assert!(hint.contains("Ctrl+J"));
    }

    #[test]
    fn newline_hint_is_shorter_when_shift_enter_is_configured() {
        let hint = newline_control_hint(DetectedTerminal::VsCode, true, 'j');

        assert!(hint.contains("Shift+Enter newline"));
        assert!(!hint.contains("/terminal-setup"));
    }

    #[test]
    fn iterm2_command_installs_escape_enter_global_keymap() {
        let command = iterm2_defaults_command();

        assert_eq!(command.program, "/usr/bin/defaults");
        assert_eq!(
            command.args,
            vec![
                "write",
                "com.googlecode.iterm2",
                "GlobalKeyMap",
                "-dict-add",
                "0xd-0x20000-0x24",
                r#"{ Action = 11; Text = "0x1b 0x0d"; Version = 1; Keycode = 13; Modifiers = 131072; }"#,
            ]
        );
    }

    #[test]
    fn vscode_keybinding_is_created_and_not_duplicated() {
        let temp = TempDir::new().unwrap();
        let first = install_vscode_keybinding(temp.path(), OsFamily::Mac).unwrap();
        let second = install_vscode_keybinding(temp.path(), OsFamily::Mac).unwrap();
        let path = vscode_keybindings_path(temp.path(), OsFamily::Mac);
        let content = std::fs::read_to_string(path).unwrap();

        assert!(matches!(first.status, InstallStatusKind::Installed));
        assert!(matches!(second.status, InstallStatusKind::AlreadyInstalled));
        assert_eq!(
            content
                .matches("workbench.action.terminal.sendSequence")
                .count(),
            1
        );
        assert!(content.contains(r#""key": "shift+enter""#));
        assert!(content.contains(r#""text": "\u001b\r""#));
    }

    #[test]
    fn vscode_invalid_keybindings_file_reports_failure_without_overwriting() {
        let temp = TempDir::new().unwrap();
        let path = vscode_keybindings_path(temp.path(), OsFamily::Mac);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();

        let err = install_vscode_keybinding(temp.path(), OsFamily::Mac).unwrap_err();

        assert!(err.to_string().contains("expected a JSON array"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}");
    }

    #[test]
    fn kitty_config_is_created_and_not_duplicated() {
        let temp = TempDir::new().unwrap();
        let first = install_kitty_keybinding(temp.path()).unwrap();
        let second = install_kitty_keybinding(temp.path()).unwrap();
        let content = std::fs::read_to_string(kitty_config_path(temp.path())).unwrap();

        assert!(matches!(first.status, InstallStatusKind::Installed));
        assert!(matches!(second.status, InstallStatusKind::AlreadyInstalled));
        assert_eq!(
            content
                .matches("map shift+enter send_text all \\x1b\\r")
                .count(),
            1
        );
    }

    #[test]
    fn alacritty_toml_binding_is_created_and_not_duplicated() {
        let temp = TempDir::new().unwrap();
        let first = install_alacritty_keybinding(temp.path()).unwrap();
        let second = install_alacritty_keybinding(temp.path()).unwrap();
        let content = std::fs::read_to_string(alacritty_config_path(temp.path())).unwrap();

        assert!(matches!(first.status, InstallStatusKind::Installed));
        assert!(matches!(second.status, InstallStatusKind::AlreadyInstalled));
        assert_eq!(content.matches("[[keyboard.bindings]]").count(), 1);
        assert!(content.contains(r#"chars = "\u001B\r""#));
        assert!(content.contains(r#"key = "Return""#));
        assert!(content.contains(r#"mods = "Shift""#));
    }

    #[test]
    fn wezterm_binding_is_inserted_before_return_config_once() {
        let temp = TempDir::new().unwrap();
        let path = wezterm_config_path(temp.path());
        std::fs::write(
            &path,
            "local wezterm = require 'wezterm'\nlocal config = {}\nreturn config\n",
        )
        .unwrap();

        let first = install_wezterm_keybinding(temp.path()).unwrap();
        let second = install_wezterm_keybinding(temp.path()).unwrap();
        let content = std::fs::read_to_string(path).unwrap();

        assert!(matches!(first.status, InstallStatusKind::Installed));
        assert!(matches!(second.status, InstallStatusKind::AlreadyInstalled));
        assert_eq!(content.matches("wezterm.action.SendString").count(), 1);
        assert!(content.find("SendString").unwrap() < content.find("return config").unwrap());
    }

    #[test]
    fn wezterm_unrecognized_config_returns_actionable_failure() {
        let temp = TempDir::new().unwrap();
        let path = wezterm_config_path(temp.path());
        std::fs::write(&path, "return {}\n").unwrap();

        let err = install_wezterm_keybinding(temp.path()).unwrap_err();

        assert!(err.to_string().contains("return config"));
        assert!(err.to_string().contains("manual"));
    }
}
