use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use console::style;
use goose::config::search_path::SearchPaths;
use goose::config::Config;
use goose::providers::generic_acp::{
    env_var_refs, generic_acp_provider_name, parse_acp_command, read_acp_agents,
    GOOSE_ACP_AGENTS_KEY,
};

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

fn roles_configured(config: &Config) -> bool {
    config.get_param::<String>("GOOSE_PLANNER_PROVIDER").is_ok()
        && config
            .get_param::<String>("GOOSE_IMPLEMENTER_PROVIDER")
            .is_ok()
}

fn gate_advisory(config: &Config) -> Option<String> {
    let gates = config
        .get_param::<Vec<String>>("GOOSE_ORCH_GATES")
        .unwrap_or_default();
    if gates.iter().any(|gate| !gate.trim().is_empty()) {
        return None;
    }

    Some(
        "orch mechanical gates not configured (GOOSE_ORCH_GATES)\n      tip: set GOOSE_ORCH_GATES=[\"cargo fmt --check\", \"cargo test -p goose-cli\"] to run fmt/tests/lint before review"
            .to_string(),
    )
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

    for check in claude_checks
        .iter()
        .chain(codex_checks.iter())
        .chain([&claude_adapter_check, &codex_adapter_check])
        .chain(generic_acp_checks.iter())
    {
        print_check(check);
    }

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

    if let Some(advisory) = gate_advisory(config) {
        let mut lines = advisory.lines();
        if let Some(label) = lines.next() {
            println!("  {} {}", style("•").yellow(), label);
        }
        for line in lines {
            println!("{}", style(line).cyan());
        }
    }

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
    fn gate_advisory_present_when_unset() {
        let config = test_config();

        let advisory = gate_advisory(&config).expect("advisory");

        assert!(advisory.contains("GOOSE_ORCH_GATES"));
        assert!(advisory.contains("cargo fmt --check"));
        assert!(advisory.contains("cargo test -p goose-cli"));
    }

    #[test]
    fn gate_advisory_absent_when_set() {
        let config = test_config();
        config
            .set_param("GOOSE_ORCH_GATES", vec!["cargo fmt --check".to_string()])
            .unwrap();

        assert!(gate_advisory(&config).is_none());
    }
}
