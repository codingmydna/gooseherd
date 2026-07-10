use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use console::style;
use goose::config::search_path::SearchPaths;
use goose::config::Config;
use goose::providers::generic_acp::{
    env_var_refs, generic_acp_provider_name, parse_acp_command, read_acp_agents,
    GOOSE_ACP_AGENTS_KEY,
};

use crate::commands::adapters::{catalog, example_model, unknown_name_error, AdapterEntry};
use crate::session::{gate_banner_line, resolve_gates, GateSource};

const CLAUDE_ADAPTER: &str = "claude-agent-acp";
const CODEX_ADAPTER: &str = "codex-acp";
const CLAUDE_ADAPTER_PKG: &str = "@agentclientprotocol/claude-agent-acp";
const CODEX_ADAPTER_PKG: &str = "@agentclientprotocol/codex-acp";
const PREMIUM_PRESET_SPEC: &str =
    "planner=claude-acp/default implementer=codex-acp/gpt-5.5 reviewer=claude-acp/default";

struct Check {
    label: String,
    ok: bool,
    detail: Option<String>,
    fix: Option<String>,
}

impl Check {
    fn ok(label: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            label: label.into(),
            ok: true,
            detail,
            fix: None,
        }
    }

    fn fail(label: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ok: false,
            detail: None,
            fix: Some(fix.into()),
        }
    }
}

fn resolve_binary(name: &str) -> Option<PathBuf> {
    SearchPaths::builder().with_npm().resolve(name).ok()
}

fn command_output(program: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
    let output = Command::new(program).args(args).output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text))
}

fn check_claude_cli() -> (Vec<Check>, bool) {
    let Some(path) = resolve_binary("claude") else {
        return (
            vec![Check::fail(
                "claude CLI on PATH",
                "npm install -g @anthropic-ai/claude-code",
            )],
            false,
        );
    };
    let mut checks = vec![Check::ok(
        "claude CLI on PATH",
        Some(path.display().to_string()),
    )];
    let logged_in = match command_output(&path, &["auth", "status"]) {
        Some((success, output)) => {
            success && !output.to_lowercase().contains("\"loggedin\": false")
        }
        None => false,
    };
    if logged_in {
        checks.push(Check::ok("claude CLI logged in", None));
    } else {
        checks.push(Check::fail(
            "claude CLI logged in",
            "run `claude` once and complete the login flow",
        ));
    }
    (checks, true)
}

fn check_codex_cli() -> (Vec<Check>, bool) {
    let Some(path) = resolve_binary("codex") else {
        return (
            vec![Check::fail(
                "codex CLI on PATH",
                "npm install -g @openai/codex",
            )],
            false,
        );
    };
    let mut checks = vec![Check::ok(
        "codex CLI on PATH",
        Some(path.display().to_string()),
    )];
    let logged_in = matches!(
        command_output(&path, &["login", "status"]),
        Some((_, output)) if output.contains("Logged in")
    );
    if logged_in {
        checks.push(Check::ok("codex CLI logged in", None));
    } else {
        checks.push(Check::fail("codex CLI logged in", "codex login"));
    }
    (checks, true)
}

fn check_adapter(binary: &str, package: &str) -> (Check, bool) {
    match resolve_binary(binary) {
        Some(path) => (
            Check::ok(
                format!("{binary} adapter"),
                Some(path.display().to_string()),
            ),
            true,
        ),
        None => (
            Check::fail(
                format!("{binary} adapter"),
                format!("npm install -g {package}"),
            ),
            false,
        ),
    }
}

fn check_generic_acp_agents(config: &Config) -> Vec<Check> {
    let agents = match read_acp_agents(config) {
        Ok(agents) => agents,
        Err(error) => {
            return vec![Check::fail(
                GOOSE_ACP_AGENTS_KEY,
                format!("fix {GOOSE_ACP_AGENTS_KEY} in your goose config: {error}"),
            )];
        }
    };

    let mut checks = Vec::new();
    for (key, spec) in agents {
        let provider_name = generic_acp_provider_name(&key);
        match parse_acp_command(spec.command()) {
            Ok((program, _)) => match resolve_binary(&program) {
                Some(path) => checks.push(Check::ok(
                    format!("{provider_name} agent ({program})"),
                    Some(path.display().to_string()),
                )),
                None => checks.push(Check::fail(
                    format!("{provider_name} agent ({program})"),
                    format!(
                        "install the ACP agent CLI for `{key}` or update {GOOSE_ACP_AGENTS_KEY}.{key}"
                    ),
                )),
            },
            Err(_) => checks.push(Check::fail(
                format!("{provider_name} agent"),
                format!(
                    "set {GOOSE_ACP_AGENTS_KEY}.{key} to a command such as `{key} --acp`"
                ),
            )),
        }

        let missing_refs = spec
            .env()
            .values()
            .flat_map(|value| env_var_refs(value))
            .filter(|name| config.get_secret::<String>(name).is_err())
            .collect::<BTreeSet<_>>();
        for name in missing_refs {
            checks.push(Check::fail(
                format!("{provider_name} env {name}"),
                format!("export {name}, or store it with `goose configure` (secret {name})"),
            ));
        }
    }

    checks
}

#[derive(Debug, PartialEq, Eq)]
enum CatalogState {
    Configured,
    InstalledNotConfigured,
    NotInstalled,
}

fn catalog_state(config: &Config, entry: &AdapterEntry) -> CatalogState {
    if read_acp_agents(config).is_ok_and(|agents| agents.contains_key(&entry.name)) {
        return CatalogState::Configured;
    }

    let installed = parse_acp_command(entry.command.command())
        .ok()
        .and_then(|(program, _)| resolve_binary(&program))
        .is_some();
    if installed {
        CatalogState::InstalledNotConfigured
    } else {
        CatalogState::NotInstalled
    }
}

fn format_catalog_entry(entry: &AdapterEntry, state: &CatalogState) -> String {
    match state {
        CatalogState::Configured => format!(
            "✓ {} — {} (configured as {})",
            entry.name,
            entry.description,
            generic_acp_provider_name(&entry.name)
        ),
        CatalogState::InstalledNotConfigured => format!(
            "○ {} — {} (installed — run `goose herd add {}`)",
            entry.name, entry.description, entry.name
        ),
        CatalogState::NotInstalled => format!(
            "✗ {} — {} (not installed — {})",
            entry.name, entry.description, entry.install
        ),
    }
}

fn print_catalog_section(config: &Config) {
    println!();
    println!("{}", style("Agent catalog:").bold());
    match catalog() {
        Ok(entries) => {
            for entry in entries.values() {
                println!(
                    "  {}",
                    format_catalog_entry(entry, &catalog_state(config, entry))
                );
            }
        }
        Err(error) => println!(
            "  {} could not load agent catalog: {error}",
            style("!").yellow()
        ),
    }
}

/// Environment diagnostics printed by `goose doctor` before any session is built,
/// so setup problems are visible even when no provider is configured. Reuses the
/// same check functions as `goose herd`.
pub fn print_environment_diagnostics(config: &Config) {
    println!("{}", style("Environment").bold());
    match config.get_goose_provider() {
        Ok(provider) => {
            let model = config
                .get_goose_model()
                .unwrap_or_else(|_| "<unset>".to_string());
            print_check(&Check::ok(
                "provider configured",
                Some(format!("{provider} / {model}")),
            ));
        }
        Err(_) => print_check(&Check::fail(
            "provider configured",
            "goose herd (recommended) or goose configure",
        )),
    }

    let (claude_checks, _) = check_claude_cli();
    let (codex_checks, _) = check_codex_cli();
    let (claude_adapter_check, _) = check_adapter(CLAUDE_ADAPTER, CLAUDE_ADAPTER_PKG);
    let (codex_adapter_check, _) = check_adapter(CODEX_ADAPTER, CODEX_ADAPTER_PKG);
    let generic_acp_checks = check_generic_acp_agents(config);
    for check in claude_checks
        .iter()
        .chain(codex_checks.iter())
        .chain([&claude_adapter_check, &codex_adapter_check])
        .chain(generic_acp_checks.iter())
    {
        print_check(check);
    }

    print_catalog_section(config);
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    AlreadyConfigured,
    Overwritten,
}

pub fn add_agent(config: &Config, name: &str, force: bool) -> Result<AddOutcome> {
    let entries = catalog()?;
    let entry = entries
        .get(name)
        .ok_or_else(|| unknown_name_error(name, &entries))?;
    let mut agents = read_acp_agents(config)?;

    if agents.contains_key(name) && !force {
        return Ok(AddOutcome::AlreadyConfigured);
    }

    let outcome = if agents
        .insert(name.to_string(), entry.command.clone())
        .is_some()
    {
        AddOutcome::Overwritten
    } else {
        AddOutcome::Added
    };
    config.set_param(GOOSE_ACP_AGENTS_KEY, &agents)?;
    Ok(outcome)
}

pub async fn handle_herd_add(name: &str, force: bool) -> Result<()> {
    let config = Config::global();
    let outcome = add_agent(config, name, force)?;
    if outcome == AddOutcome::AlreadyConfigured {
        println!(
            "{} is already configured as {}. Use --force to overwrite it.",
            name,
            generic_acp_provider_name(name)
        );
        return Ok(());
    }

    let entries = catalog()?;
    let entry = &entries[name];
    println!(
        "{} configured {} as {}.",
        style("✓").green(),
        name,
        generic_acp_provider_name(name)
    );
    println!();
    println!("{}", style("Next steps:").bold());
    if parse_acp_command(entry.command.command())
        .ok()
        .and_then(|(program, _)| resolve_binary(&program))
        .is_none()
    {
        println!("  install: {}", entry.install);
    }
    println!("  authenticate: {}", entry.auth);

    let env_refs = entry
        .command
        .env()
        .values()
        .flat_map(|value| env_var_refs(value))
        .collect::<BTreeSet<_>>();
    for env_ref in env_refs {
        println!("  environment: export {env_ref}, or store it with `goose configure`");
    }
    println!(
        "  assign a role: /roles implementer={}/{}",
        generic_acp_provider_name(name),
        example_model(entry)
    );
    println!("  verify: goose herd");
    Ok(())
}

fn roles_configured(config: &Config) -> bool {
    config.get_param::<String>("GOOSE_PLANNER_PROVIDER").is_ok()
        && config
            .get_param::<String>("GOOSE_IMPLEMENTER_PROVIDER")
            .is_ok()
}

fn repo_gate_check(config: &Config, repo_dir: &Path) -> Check {
    let global_gates = config
        .get_param::<Vec<String>>("GOOSE_ORCH_GATES")
        .unwrap_or_default();
    let resolved = resolve_gates(repo_dir, None, global_gates);
    let detail = gate_banner_line(&resolved)
        .strip_prefix("gates: ")
        .unwrap_or_default()
        .to_string();

    if let Some(warning) = resolved.warning {
        Check {
            label: "repo gates".to_string(),
            ok: false,
            detail: Some(detail),
            fix: Some(warning),
        }
    } else if !resolved.gates.is_empty() || matches!(resolved.source, GateSource::LocalFile(_)) {
        Check::ok("repo gates", Some(detail))
    } else {
        Check::fail(
            "repo gates",
            "commit .goose-gates.yaml with gate commands, or set GOOSE_ORCH_GATES; package.json and go.mod gates are derived automatically",
        )
    }
}

const LOCAL_GATES_FILE: &str = ".goose-gates.yaml";

/// The primary crate to scope a `cargo test` starter gate to, so it stays fast in
/// a workspace. Prefers `[package].name`, then the first `default-members` entry,
/// then the first `members` entry.
fn primary_cargo_crate(repo_dir: &Path) -> Option<String> {
    let root = std::fs::read_to_string(repo_dir.join("Cargo.toml")).ok()?;
    if let Some(name) = package_name(&root) {
        return Some(name);
    }
    let member = first_workspace_member(&root)?;
    let member_path = repo_dir.join(&member);
    std::fs::read_to_string(member_path.join("Cargo.toml"))
        .ok()
        .and_then(|contents| package_name(&contents))
        .or_else(|| {
            Path::new(&member)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
}

fn package_name(cargo_toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = trimmed.strip_prefix("name") {
                if let Some(value) = value.trim_start().strip_prefix('=') {
                    let name = value.trim().trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

fn first_workspace_member(cargo_toml: &str) -> Option<String> {
    ["default-members", "members"]
        .into_iter()
        .find_map(|key| first_member_for_key(cargo_toml, key))
}

fn first_member_for_key(cargo_toml: &str, key: &str) -> Option<String> {
    let mut collecting = false;
    let mut buffer = String::new();
    for line in cargo_toml.lines() {
        let trimmed = line.trim_start();
        if !collecting {
            let Some(rest) = trimmed.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            buffer.push_str(rest);
            collecting = true;
        } else {
            buffer.push(' ');
            buffer.push_str(trimmed);
        }
        if buffer.contains(']') {
            break;
        }
    }
    if !collecting {
        return None;
    }
    buffer
        .split('"')
        .nth(1)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
}

/// Starter machine gates for a Cargo repo. Cargo gates are never auto-derived
/// (only package.json/go.mod are), so a fresh Rust repo has no gates until the
/// user commits `.goose-gates.yaml` — this offers a fast, correct default.
fn starter_cargo_gates(repo_dir: &Path) -> Option<Vec<String>> {
    if !repo_dir.join("Cargo.toml").is_file() {
        return None;
    }
    let test = match primary_cargo_crate(repo_dir) {
        Some(crate_name) => format!("cargo test -p {crate_name} --lib"),
        None => "cargo test --lib".to_string(),
    };
    Some(vec![
        "cargo fmt --check".to_string(),
        "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
        test,
    ])
}

fn maybe_offer_starter_gates(repo_dir: &Path) -> Result<()> {
    let gates_path = repo_dir.join(LOCAL_GATES_FILE);
    if gates_path.exists() {
        return Ok(());
    }
    let Some(gates) = starter_cargo_gates(repo_dir) else {
        return Ok(());
    };
    let yaml = serde_yaml::to_string(&gates)?;

    println!();
    if std::io::stdin().is_terminal() {
        println!(
            "This looks like a Cargo repo with no {LOCAL_GATES_FILE}. A starter gates file \
             would run before every reviewer:"
        );
        for gate in &gates {
            println!("  - {gate}");
        }
        let write = cliclack::confirm(format!("Write {LOCAL_GATES_FILE}?"))
            .initial_value(true)
            .interact()?;
        if write {
            std::fs::write(&gates_path, yaml)?;
            println!("  {} wrote {}", style("✓").green(), gates_path.display());
        }
    } else {
        println!(
            "No {LOCAL_GATES_FILE} in this Cargo repo. Suggested starter (write it to the repo root):"
        );
        print!("{yaml}");
    }
    Ok(())
}

fn print_check(check: &Check) {
    let mark = if check.ok {
        style("✓").green()
    } else {
        style("✗").red()
    };
    match &check.detail {
        Some(detail) => println!(
            "  {} {} {}",
            mark,
            check.label,
            style(format!("({detail})")).dim()
        ),
        None => println!("  {} {}", mark, check.label),
    }
    if let Some(fix) = &check.fix {
        println!("      fix: {}", style(fix).cyan());
    }
}

fn write_recommended_config(config: &Config) -> Result<()> {
    config.set_param("GOOSE_PLANNER_PROVIDER", "claude-acp")?;
    config.set_param("GOOSE_PLANNER_MODEL", "default")?;
    config.set_param("GOOSE_REVIEWER_PROVIDER", "claude-acp")?;
    config.set_param("GOOSE_REVIEWER_MODEL", "default")?;
    config.set_param("GOOSE_IMPLEMENTER_PROVIDER", "codex-acp")?;
    config.set_param("GOOSE_IMPLEMENTER_MODEL", "gpt-5.5")?;
    config.set_param("GOOSE_ORCH_MAX_CYCLES", 3)?;

    let mut presets: BTreeMap<String, String> =
        config.get_param("GOOSE_ROLE_PRESETS").unwrap_or_default();
    presets.insert("premium".to_string(), PREMIUM_PRESET_SPEC.to_string());
    config.set_param("GOOSE_ROLE_PRESETS", &presets)?;
    Ok(())
}

pub async fn handle_herd() -> Result<()> {
    println!("{}", style("goose herd — onboarding check-up").bold());
    println!();

    let (claude_checks, claude_found) = check_claude_cli();
    let (codex_checks, codex_found) = check_codex_cli();
    let (claude_adapter_check, claude_adapter_found) =
        check_adapter(CLAUDE_ADAPTER, CLAUDE_ADAPTER_PKG);
    let (codex_adapter_check, codex_adapter_found) =
        check_adapter(CODEX_ADAPTER, CODEX_ADAPTER_PKG);

    let config = Config::global();
    let generic_acp_checks = check_generic_acp_agents(config);
    let mut roles_ok = roles_configured(config);
    let repo_dir = std::env::current_dir()?;

    for check in claude_checks
        .iter()
        .chain(codex_checks.iter())
        .chain([&claude_adapter_check, &codex_adapter_check])
        .chain(generic_acp_checks.iter())
    {
        print_check(check);
    }

    print_catalog_section(config);

    let role_check = if roles_ok {
        let planner: String = config.get_param("GOOSE_PLANNER_PROVIDER")?;
        let implementer: String = config.get_param("GOOSE_IMPLEMENTER_PROVIDER")?;
        Check::ok(
            "role config",
            Some(format!("planner={planner}, implementer={implementer}")),
        )
    } else {
        Check::fail(
            "role config (GOOSE_PLANNER_PROVIDER / GOOSE_IMPLEMENTER_PROVIDER)",
            "goose herd (accept the recommended config below), or set the params in your goose config file",
        )
    };
    print_check(&role_check);

    print_check(&repo_gate_check(config, &repo_dir));

    let both_vendors_available =
        claude_found && codex_found && claude_adapter_found && codex_adapter_found;

    if !roles_ok && both_vendors_available {
        println!();
        if std::io::stdin().is_terminal() {
            let write = cliclack::confirm(
                "No role config found. Write the recommended split-role setup? \
                 (planner/reviewer=claude-acp, implementer=codex-acp/gpt-5.5, max 3 cycles, saved as preset \"premium\")",
            )
            .initial_value(true)
            .interact()?;
            if write {
                write_recommended_config(config)?;
                roles_ok = true;
                println!(
                    "  {} wrote split-role config and saved preset {}",
                    style("✓").green(),
                    style("premium").cyan()
                );
            }
        } else {
            println!(
                "Not a terminal — skipping config prompt. Re-run `goose herd` interactively \
                 to write the recommended split-role config."
            );
        }
    }

    maybe_offer_starter_gates(&repo_dir)?;

    println!();
    println!("{}", style("Next steps:").bold());
    println!("  goose session      start an interactive session");
    println!("  /orch <task>       run the plan → implement → review loop");
    println!("  /status            show current role assignments");
    println!("  /preset            list or switch role presets");
    if !roles_ok || !both_vendors_available {
        println!();
        println!("Fix the ✗ items above, then run `goose herd` again.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use goose::providers::generic_acp::AcpAgentSpec;

    fn test_config() -> Config {
        let config_file = tempfile::NamedTempFile::new().unwrap();
        let secrets_file = tempfile::NamedTempFile::new().unwrap();
        Config::new_with_file_secrets(config_file.path(), secrets_file.path()).unwrap()
    }

    #[test]
    fn generic_acp_checks_report_configured_binary() {
        let config = test_config();
        #[cfg(unix)]
        let command = "sh --acp";
        #[cfg(windows)]
        let command = "cmd /C acp";
        let agents = BTreeMap::from([(
            "fake".to_string(),
            AcpAgentSpec::Command(command.to_string()),
        )]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let checks = check_generic_acp_agents(&config);

        assert_eq!(checks.len(), 1);
        assert!(checks[0].ok);
        assert!(checks[0].label.starts_with("fake-acp agent"));
    }

    #[test]
    fn generic_acp_checks_report_missing_binary() {
        let config = test_config();
        let agents = BTreeMap::from([(
            "missing".to_string(),
            AcpAgentSpec::Command("missing-acp-binary-for-goose-herd-test --acp".to_string()),
        )]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let checks = check_generic_acp_agents(&config);

        assert_eq!(checks.len(), 1);
        assert!(!checks[0].ok);
        assert!(checks[0].label.contains("missing-acp agent"));
        assert!(checks[0]
            .fix
            .as_ref()
            .unwrap()
            .contains("install the ACP agent CLI"));
    }

    #[test]
    fn generic_acp_checks_report_invalid_command() {
        let config = test_config();
        let agents = BTreeMap::from([(
            "empty".to_string(),
            AcpAgentSpec::Command("   ".to_string()),
        )]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let checks = check_generic_acp_agents(&config);

        assert_eq!(checks.len(), 1);
        assert!(!checks[0].ok);
        assert_eq!(checks[0].label, "empty-acp agent");
    }

    #[test]
    fn generic_acp_checks_report_missing_env_reference() {
        let config = test_config();
        #[cfg(unix)]
        let command = "sh --acp";
        #[cfg(windows)]
        let command = "cmd /C acp";
        let agents = BTreeMap::from([(
            "glm".to_string(),
            AcpAgentSpec::Detailed {
                command: command.to_string(),
                env: BTreeMap::from([(
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "${GOOSE_TEST_HERD_MISSING_ZAI_KEY}".to_string(),
                )]),
                env_remove: vec![],
            },
        )]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let checks = check_generic_acp_agents(&config);

        let missing = checks
            .iter()
            .find(|check| check.label.contains("GOOSE_TEST_HERD_MISSING_ZAI_KEY"))
            .expect("missing env advisory");
        assert!(!missing.ok);
        assert!(missing.fix.as_ref().unwrap().contains("goose configure"));
        assert!(!missing.label.contains("dummy"));
        assert!(!missing.fix.as_ref().unwrap().contains("dummy"));
    }

    #[test]
    fn generic_acp_checks_skip_env_reference_advisory_when_secret_exists() {
        let config = test_config();
        config
            .set_secret("GOOSE_TEST_HERD_PRESENT_ZAI_KEY", &"dummy")
            .unwrap();
        #[cfg(unix)]
        let command = "sh --acp";
        #[cfg(windows)]
        let command = "cmd /C acp";
        let agents = BTreeMap::from([(
            "glm".to_string(),
            AcpAgentSpec::Detailed {
                command: command.to_string(),
                env: BTreeMap::from([(
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    "${GOOSE_TEST_HERD_PRESENT_ZAI_KEY}".to_string(),
                )]),
                env_remove: vec![],
            },
        )]);
        config.set_param(GOOSE_ACP_AGENTS_KEY, &agents).unwrap();

        let checks = check_generic_acp_agents(&config);

        assert!(!checks
            .iter()
            .any(|check| check.label.contains("GOOSE_TEST_HERD_PRESENT_ZAI_KEY")));
        assert!(!checks.iter().any(|check| check
            .fix
            .as_deref()
            .unwrap_or_default()
            .contains("dummy")));
    }

    #[test]
    fn repo_gate_check_fails_when_no_gates_are_available() {
        let config = test_config();
        let temp = tempfile::tempdir().expect("tempdir");

        let check = repo_gate_check(&config, temp.path());

        assert!(!check.ok);
        assert!(check
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains(".goose-gates.yaml")));
        assert!(check
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("GOOSE_ORCH_GATES")));
    }

    #[test]
    fn repo_gate_check_reports_global_gates() {
        let config = test_config();
        let temp = tempfile::tempdir().expect("tempdir");
        config
            .set_param("GOOSE_ORCH_GATES", vec!["cargo fmt --check".to_string()])
            .unwrap();

        let check = repo_gate_check(&config, temp.path());

        assert!(check.ok);
        assert!(check
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("global GOOSE_ORCH_GATES")));
    }

    #[test]
    fn repo_gate_check_reports_invalid_local_file() {
        let config = test_config();
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join(".goose-gates.yaml"), "command: test\n")
            .expect("write local gates");

        let check = repo_gate_check(&config, temp.path());

        assert!(!check.ok);
        assert!(check
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("could not parse")));
    }

    #[test]
    fn add_agent_to_empty_config() {
        let config = test_config();

        assert_eq!(
            add_agent(&config, "gemini", false).unwrap(),
            AddOutcome::Added
        );
        assert_eq!(
            read_acp_agents(&config).unwrap()["gemini"],
            AcpAgentSpec::Command("gemini --acp".to_string())
        );
    }

    #[test]
    fn add_agent_preserves_existing_entries() {
        let config = test_config();
        let custom = AcpAgentSpec::Command("custom-agent --acp".to_string());
        config
            .set_param(
                GOOSE_ACP_AGENTS_KEY,
                BTreeMap::from([("mycustom".to_string(), custom.clone())]),
            )
            .unwrap();

        assert_eq!(
            add_agent(&config, "kimi", false).unwrap(),
            AddOutcome::Added
        );
        let agents = read_acp_agents(&config).unwrap();
        assert_eq!(agents["mycustom"], custom);
        assert_eq!(agents["kimi"].command(), "kimi acp");
    }

    #[test]
    fn add_agent_duplicate_is_a_no_op() {
        let config = test_config();
        let custom = AcpAgentSpec::Command("my-gemini --acp".to_string());
        config
            .set_param(
                GOOSE_ACP_AGENTS_KEY,
                BTreeMap::from([("gemini".to_string(), custom.clone())]),
            )
            .unwrap();

        assert_eq!(
            add_agent(&config, "gemini", false).unwrap(),
            AddOutcome::AlreadyConfigured
        );
        assert_eq!(read_acp_agents(&config).unwrap()["gemini"], custom);
    }

    #[test]
    fn add_agent_force_overwrites_existing_entry() {
        let config = test_config();
        config
            .set_param(
                GOOSE_ACP_AGENTS_KEY,
                BTreeMap::from([(
                    "gemini".to_string(),
                    AcpAgentSpec::Command("my-gemini --acp".to_string()),
                )]),
            )
            .unwrap();

        assert_eq!(
            add_agent(&config, "gemini", true).unwrap(),
            AddOutcome::Overwritten
        );
        assert_eq!(
            read_acp_agents(&config).unwrap()["gemini"],
            AcpAgentSpec::Command("gemini --acp".to_string())
        );
    }

    #[test]
    fn add_agent_unknown_name_lists_catalog() {
        let message = add_agent(&test_config(), "missing", false)
            .unwrap_err()
            .to_string();

        assert!(message.contains("unknown agent `missing`"));
        for name in ["gemini", "glm", "kimi", "opencode", "vibe"] {
            assert!(message.contains(name));
        }
    }

    #[test]
    fn formats_catalog_states() {
        let entry = catalog().unwrap().remove("gemini").unwrap();

        assert_eq!(
            format_catalog_entry(&entry, &CatalogState::Configured),
            "✓ gemini — Google Gemini CLI (configured as gemini-acp)"
        );
        assert_eq!(
            format_catalog_entry(&entry, &CatalogState::InstalledNotConfigured),
            "○ gemini — Google Gemini CLI (installed — run `goose herd add gemini`)"
        );
        assert_eq!(
            format_catalog_entry(&entry, &CatalogState::NotInstalled),
            "✗ gemini — Google Gemini CLI (not installed — npm install -g @google/gemini-cli)"
        );
    }

    #[test]
    fn primary_cargo_crate_uses_package_name() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        assert_eq!(primary_cargo_crate(temp.path()).as_deref(), Some("solo"));
    }

    #[test]
    fn primary_cargo_crate_prefers_default_members() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\n  \"crates/other\",\n  \"crates/app\",\n]\ndefault-members = [\"crates/app\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("crates/app")).unwrap();
        std::fs::write(
            temp.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"myapp\"\n",
        )
        .unwrap();

        assert_eq!(primary_cargo_crate(temp.path()).as_deref(), Some("myapp"));
    }

    #[test]
    fn primary_cargo_crate_falls_back_to_first_member() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/first\", \"crates/second\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("crates/first")).unwrap();
        std::fs::write(
            temp.path().join("crates/first/Cargo.toml"),
            "[package]\nname = \"firstcrate\"\n",
        )
        .unwrap();

        assert_eq!(
            primary_cargo_crate(temp.path()).as_deref(),
            Some("firstcrate")
        );
    }

    #[test]
    fn starter_cargo_gates_scopes_test_to_primary_crate() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"solo\"\n",
        )
        .unwrap();

        let gates = starter_cargo_gates(temp.path()).unwrap();

        assert_eq!(
            gates,
            vec![
                "cargo fmt --check",
                "cargo clippy --workspace --all-targets -- -D warnings",
                "cargo test -p solo --lib",
            ]
        );
    }

    #[test]
    fn starter_cargo_gates_none_without_cargo_toml() {
        let temp = tempfile::tempdir().unwrap();
        assert!(starter_cargo_gates(temp.path()).is_none());
    }
}
