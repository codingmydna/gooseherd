use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use console::style;
use goose::config::search_path::SearchPaths;
use goose::config::Config;

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

fn roles_configured(config: &Config) -> bool {
    config.get_param::<String>("GOOSE_PLANNER_PROVIDER").is_ok()
        && config
            .get_param::<String>("GOOSE_IMPLEMENTER_PROVIDER")
            .is_ok()
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
    let mut roles_ok = roles_configured(config);

    for check in claude_checks
        .iter()
        .chain(codex_checks.iter())
        .chain([&claude_adapter_check, &codex_adapter_check])
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
