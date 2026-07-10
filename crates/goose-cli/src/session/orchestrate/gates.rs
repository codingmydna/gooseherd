use crate::session::ledger;
use goose::config::Config;
use goose::utils::safe_truncate;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use super::phases::PhaseMeta;

const GATE_OUTPUT_TAIL_LIMIT: usize = 4_000;
const LOCAL_GATES_FILE: &str = ".goose-gates.yaml";
const GATE_TIMEOUT_KEY: &str = "GOOSE_ORCH_GATE_TIMEOUT_SECS";
const GATE_ENV_KEY: &str = "GOOSE_ORCH_GATE_ENV";
const DEFAULT_GATE_TIMEOUT_SECS: u64 = 3600;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateSource {
    LocalFile(PathBuf),
    Derived {
        manifest: &'static str,
        detail: String,
    },
    Global,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResolvedGates {
    pub(crate) source: GateSource,
    pub(crate) gates: Vec<String>,
    pub(crate) warning: Option<String>,
}

enum LocalGates {
    Missing,
    Loaded(Vec<String>),
    Invalid(String),
}

pub(crate) fn resolve_gates(
    impl_dir: &Path,
    fallback_dir: Option<&Path>,
    global_gates: Vec<String>,
) -> ResolvedGates {
    let mut warning = None;
    let local_dirs = std::iter::once(impl_dir).chain(fallback_dir.filter(|dir| *dir != impl_dir));
    for dir in local_dirs {
        match load_local_gates(dir) {
            LocalGates::Missing => {}
            LocalGates::Loaded(gates) => {
                return ResolvedGates {
                    source: GateSource::LocalFile(dir.join(LOCAL_GATES_FILE)),
                    gates: effective_gates(gates),
                    warning,
                };
            }
            LocalGates::Invalid(error) => {
                warning = Some(error);
                break;
            }
        }
    }

    if let Some((source, gates)) = derive_gates(impl_dir) {
        if !gates.is_empty() {
            return ResolvedGates {
                source,
                gates,
                warning,
            };
        }
    }

    let global_gates = effective_gates(global_gates);
    if !global_gates.is_empty() {
        return ResolvedGates {
            source: GateSource::Global,
            gates: global_gates,
            warning,
        };
    }

    ResolvedGates {
        source: GateSource::None,
        gates: Vec::new(),
        warning,
    }
}

fn load_local_gates(dir: &Path) -> LocalGates {
    let path = dir.join(LOCAL_GATES_FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LocalGates::Missing,
        Err(error) => {
            return LocalGates::Invalid(format!(
                "could not read {}: {error}; deriving repo gates instead",
                path.display()
            ));
        }
    };

    match serde_yaml::from_str::<Vec<String>>(&contents) {
        Ok(gates) => LocalGates::Loaded(gates),
        Err(error) => LocalGates::Invalid(format!(
            "could not parse {}: {error}; deriving repo gates instead",
            path.display()
        )),
    }
}

fn derive_gates(dir: &Path) -> Option<(GateSource, Vec<String>)> {
    if dir.join("Cargo.toml").is_file() {
        return None;
    }
    if dir.join("package.json").is_file() {
        let (manager, lockfile) = detect_js_package_manager(dir);
        return Some((
            GateSource::Derived {
                manifest: "package.json",
                detail: lockfile.to_string(),
            },
            derive_js_gates(dir, manager),
        ));
    }
    if dir.join("go.mod").is_file() {
        return Some((
            GateSource::Derived {
                manifest: "go.mod",
                detail: String::new(),
            },
            vec!["go build ./...".to_string(), "go test ./...".to_string()],
        ));
    }
    None
}

fn detect_js_package_manager(dir: &Path) -> (&'static str, &'static str) {
    if dir.join("pnpm-lock.yaml").is_file() {
        ("pnpm", "pnpm-lock.yaml")
    } else if dir.join("yarn.lock").is_file() {
        ("yarn", "yarn.lock")
    } else if dir.join("package-lock.json").is_file() {
        ("npm", "package-lock.json")
    } else {
        ("npm", "no lockfile")
    }
}

fn derive_js_gates(dir: &Path, manager: &str) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(dir.join("package.json")) else {
        return Vec::new();
    };
    let Ok(package) = serde_json::from_str::<Value>(&contents) else {
        return Vec::new();
    };
    let Some(scripts) = package.get("scripts").and_then(Value::as_object) else {
        return Vec::new();
    };

    ["test", "build"]
        .into_iter()
        .filter(|name| {
            scripts
                .get(*name)
                .and_then(Value::as_str)
                .is_some_and(|script| {
                    !script.trim().is_empty()
                        && !script.to_ascii_lowercase().contains("no test specified")
                })
        })
        .map(|name| format!("{manager} run {name}"))
        .collect()
}

pub(crate) fn gate_banner_line(resolved: &ResolvedGates) -> String {
    let commands = if resolved.gates.is_empty() {
        String::new()
    } else {
        format!(" — {}", resolved.gates.join("; "))
    };
    match &resolved.source {
        GateSource::LocalFile(path) => format!(
            "gates: {} ({}){commands}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(LOCAL_GATES_FILE),
            resolved.gates.len()
        ),
        GateSource::Derived { manifest, detail } if detail.is_empty() => {
            format!("gates: derived from {manifest}{commands}")
        }
        GateSource::Derived { manifest, detail } => {
            format!("gates: derived from {manifest} + {detail}{commands}")
        }
        GateSource::Global => format!(
            "gates: global GOOSE_ORCH_GATES ({}){commands}",
            resolved.gates.len()
        ),
        GateSource::None => {
            "gates: none — no .goose-gates.yaml, no derivable manifest, GOOSE_ORCH_GATES unset"
                .to_string()
        }
    }
}

/// One-line, override-pointing notice printed the first time a repo's gates are
/// derived from a manifest (rather than an explicit `.goose-gates.yaml` or
/// `GOOSE_ORCH_GATES`). Headless, so it's visibility only — no prompt.
pub(crate) fn derived_gates_notice(resolved: &ResolvedGates) -> Option<String> {
    match &resolved.source {
        GateSource::Derived { manifest, .. } => Some(format!(
            "gates derived from {manifest} — set GOOSE_ORCH_GATES or .goose-gates.yaml to override"
        )),
        _ => None,
    }
}

/// Default command allowlist for a headless implement run: git plus the repo's
/// detected build tools and the first token of each derivable gate command, so
/// the implementer can build/test/commit while shell-chaining and unlisted
/// programs stay denied. Reuses the same manifest detection as gate derivation.
pub(crate) fn seed_allowed_commands(impl_dir: &Path, resolved: &ResolvedGates) -> Vec<String> {
    let mut seed = vec!["git".to_string()];
    if impl_dir.join("Cargo.toml").is_file() {
        seed.push("cargo".to_string());
    }
    if impl_dir.join("package.json").is_file() {
        let (manager, _) = detect_js_package_manager(impl_dir);
        seed.push(manager.to_string());
        seed.push("npx".to_string());
        seed.push("node".to_string());
    }
    if impl_dir.join("go.mod").is_file() {
        seed.push("go".to_string());
    }
    for gate in &resolved.gates {
        if command_needs_shell(gate) {
            continue;
        }
        if let Some(token) = gate.split_whitespace().next() {
            seed.push(token.to_string());
        }
    }
    seed.sort();
    seed.dedup();
    seed
}

#[derive(Debug, Clone)]
pub(super) struct GateRun {
    pub command: String,
    pub output_tail: String,
}

#[derive(Debug)]
pub(super) enum GateOutcome {
    Passed {
        runs: Vec<GateRun>,
    },
    Failed {
        command: String,
        output_tail: String,
    },
}

#[derive(Debug)]
pub(super) enum GateStep {
    Proceed,
    Reimplement(String),
    Abort(String),
}

pub(super) fn effective_gates(gates: Vec<String>) -> Vec<String> {
    gates
        .into_iter()
        .map(|gate| gate.trim().to_string())
        .filter(|gate| !gate.is_empty())
        .collect()
}

#[derive(Debug)]
pub(super) struct GateSkip {
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub(super) struct GatePartition {
    pub applicable: Vec<String>,
    pub skipped: Vec<GateSkip>,
}

/// Split gates into those that can run against `impl_dir` and those that must be
/// skipped. A gate is skipped when its build tool's manifest is absent from the
/// target repo (a `cargo` gate in a repo with no `Cargo.toml`) or the tool is
/// not installed. Without this, an unrelated gate fails forever and re-dispatches
/// the implementer — which is how orchestrating a JS repo with goose's own Rust
/// gates looped and pushed the implementer to fabricate a `Cargo.toml`.
pub(super) fn partition_gates(impl_dir: &Path, gates: &[String]) -> GatePartition {
    let mut partition = GatePartition::default();
    for gate in gates {
        let command = gate.trim();
        if command.is_empty() {
            continue;
        }
        match gate_skip_reason(impl_dir, command) {
            Some(reason) => partition.skipped.push(GateSkip {
                command: command.to_string(),
                reason,
            }),
            None => partition.applicable.push(command.to_string()),
        }
    }
    partition
}

/// Build tools that do nothing useful without their manifest in the repo root.
fn required_manifest(program: &str) -> Option<&'static [&'static str]> {
    match program {
        "cargo" | "rustc" | "rustfmt" | "clippy-driver" => Some(&["Cargo.toml"]),
        "pnpm" | "npm" | "yarn" | "npx" | "node" => Some(&["package.json"]),
        "go" | "gofmt" => Some(&["go.mod"]),
        _ => None,
    }
}

fn program_on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// `Some(reason)` when a gate can't meaningfully run against `impl_dir`. Shell-form
/// gates are always kept — they are user-authored and may guard/`cd` themselves.
fn gate_skip_reason(impl_dir: &Path, command: &str) -> Option<String> {
    if command_needs_shell(command) {
        return None;
    }
    let program = command.split_whitespace().next()?;
    if let Some(manifests) = required_manifest(program) {
        let present = manifests.iter().any(|name| impl_dir.join(name).exists());
        if !present {
            return Some(format!("no {} in the target repo", manifests.join(" or ")));
        }
    }
    if !program_on_path(program) {
        return Some(format!("`{program}` is not installed"));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateEnvMode {
    Scrub,
    Inherit,
}

fn gate_timeout() -> Duration {
    let secs = Config::global()
        .get_param::<u64>(GATE_TIMEOUT_KEY)
        .ok()
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_GATE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

pub(super) fn gate_env_mode() -> GateEnvMode {
    match Config::global().get_param::<String>(GATE_ENV_KEY) {
        Ok(raw) if raw.trim().eq_ignore_ascii_case("inherit") => GateEnvMode::Inherit,
        _ => GateEnvMode::Scrub,
    }
}

/// Environment variables that carry credentials. In scrub mode they are removed
/// from a gate's environment so repo-derived commands can't exfiltrate secrets.
pub(super) fn is_secret_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.ends_with("_API_KEY")
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_SECRET")
        || upper.ends_with("_SECRET_KEY")
        || upper.ends_with("_ACCESS_KEY")
        || upper.ends_with("_PASSWORD")
        || upper.starts_with("ANTHROPIC_")
        || upper.starts_with("OPENAI_")
        || upper.starts_with("AWS_")
        || matches!(
            upper.as_str(),
            "OPENAI_API_KEY"
                | "ANTHROPIC_API_KEY"
                | "GEMINI_API_KEY"
                | "GOOGLE_API_KEY"
                | "GROQ_API_KEY"
                | "OPENROUTER_API_KEY"
                | "DEEPSEEK_API_KEY"
                | "GITHUB_TOKEN"
                | "GH_TOKEN"
                | "HF_TOKEN"
                | "TAVILY_API_KEY"
        )
}

pub(super) async fn run_gates(impl_dir: &Path, gates: &[String]) -> GateOutcome {
    let timeout = gate_timeout();
    let env_mode = gate_env_mode();
    let mut runs = Vec::new();
    for command in gates {
        if command.trim().is_empty() {
            continue;
        }
        let output = match spawn_gate(impl_dir, command, timeout, env_mode).await {
            Ok(GateSpawn::Completed(output)) => output,
            Ok(GateSpawn::TimedOut) => {
                return GateOutcome::Failed {
                    command: command.clone(),
                    output_tail: format!(
                        "timed out after {}s (raise {} if this gate legitimately needs longer, e.g. a cold build)",
                        timeout.as_secs(),
                        GATE_TIMEOUT_KEY
                    ),
                };
            }
            Err(error) => {
                return GateOutcome::Failed {
                    command: command.clone(),
                    output_tail: format!("failed to launch gate command: {error}"),
                };
            }
        };

        let combined = combined_gate_output(&output);
        if !output.status.success() {
            let mut output_tail = tail_truncate(&combined, GATE_OUTPUT_TAIL_LIMIT);
            // A default-scrubbed env drops *_PASSWORD/_TOKEN/_SECRET and AWS_*,
            // so a gate that needs service credentials fails with an opaque auth
            // error; point the user at the escape hatch.
            if env_mode == GateEnvMode::Scrub {
                output_tail.push_str(&format!(
                    "\n\n(gate env is scrubbed by default; set {}=inherit if this gate needs credentials)",
                    GATE_ENV_KEY
                ));
            }
            return GateOutcome::Failed {
                command: command.clone(),
                output_tail,
            };
        }
        runs.push(GateRun {
            command: command.clone(),
            output_tail: tail_truncate(&combined, GATE_OUTPUT_TAIL_LIMIT),
        });
    }

    GateOutcome::Passed { runs }
}

fn combined_gate_output(output: &std::process::Output) -> String {
    let mut combined = format!("status: {}\n", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        combined.push_str(&format!("stdout:\n{stdout}\n"));
    }
    if !stderr.trim().is_empty() {
        combined.push_str(&format!("stderr:\n{stderr}\n"));
    }
    combined
}

/// Format the passing gate outputs for the review request: each command with the
/// last `tail_lines` lines of its output, so the reviewer sees the actual gate
/// results (test counts, warnings) rather than just "gates passed".
pub(super) fn gate_outputs_review_section(runs: &[GateRun], tail_lines: usize) -> String {
    if runs.is_empty() {
        return String::new();
    }
    let mut section = String::from("Gate outputs (tail):\n");
    for run in runs {
        let lines: Vec<&str> = run.output_tail.lines().collect();
        let start = lines.len().saturating_sub(tail_lines);
        section.push_str(&format!(
            "$ {}\n{}\n\n",
            run.command,
            lines[start..].join("\n")
        ));
    }
    section.trim_end().to_string()
}

enum GateSpawn {
    Completed(std::process::Output),
    TimedOut,
}

async fn spawn_gate(
    impl_dir: &Path,
    command: &str,
    timeout: Duration,
    env_mode: GateEnvMode,
) -> std::io::Result<GateSpawn> {
    use tokio::process::Command;

    let mut cmd = if command_needs_shell(command) {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    } else {
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty gate command")
        })?;
        let mut cmd = Command::new(program);
        cmd.args(parts);
        cmd
    };

    cmd.current_dir(impl_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if env_mode == GateEnvMode::Scrub {
        cmd.env_clear();
        for (key, value) in std::env::vars() {
            if !is_secret_env_key(&key) {
                cmd.env(key, value);
            }
        }
    }

    // Own process group so a timeout can kill the whole tree (a shell-form gate
    // and its children), not just the direct child.
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(true);

    let child = cmd.spawn()?;
    let pid = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => Ok(GateSpawn::Completed(result?)),
        Err(_elapsed) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // Negative pid signals the whole process group.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = pid;
            Ok(GateSpawn::TimedOut)
        }
    }
}

fn command_needs_shell(command: &str) -> bool {
    command
        .chars()
        .any(|ch| matches!(ch, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`'))
}

fn tail_truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    let marker = "...";
    let marker_len = marker.chars().count();
    if max_chars <= marker_len {
        return marker.chars().take(max_chars).collect();
    }

    let tail_len = max_chars - marker_len;
    let tail: String = s.chars().skip(count - tail_len).collect();
    format!("{marker}{tail}")
}

pub(super) fn next_gate_step(
    outcome: GateOutcome,
    gate_retries: &mut u32,
    max_gate_retries: u32,
) -> GateStep {
    match outcome {
        GateOutcome::Passed { .. } => GateStep::Proceed,
        GateOutcome::Failed {
            command,
            output_tail,
        } => {
            if *gate_retries >= max_gate_retries {
                GateStep::Abort(format!(
                    "machine gate `{command}` still failing after {max_gate_retries} retries"
                ))
            } else {
                *gate_retries += 1;
                GateStep::Reimplement(gate_rejection_instruction(&command, &output_tail))
            }
        }
    }
}

fn gate_rejection_instruction(command: &str, output_tail: &str) -> String {
    format!(
        "A machine quality gate failed; the reviewer was not called. Fix the underlying issue and re-run your verification. All gates must pass before review.\n\nFailed gate command:\n{command}\n\nGate output (tail):\n{output_tail}"
    )
}

pub(super) fn gate_passed_review_note(gates: &[String]) -> String {
    let commands = gates
        .iter()
        .map(|gate| gate.trim())
        .filter(|gate| !gate.is_empty())
        .collect::<Vec<_>>();
    if commands.is_empty() {
        String::new()
    } else {
        format!("\n\ngates passed: {}", commands.join("; "))
    }
}

pub(super) fn record_gate_phase(
    meta: &PhaseMeta<'_>,
    cycle: u32,
    passed: bool,
    detail: &str,
    elapsed_ms: u64,
) {
    let verdict = if passed {
        "PASS".to_string()
    } else {
        format!("FAIL: {detail}")
    };
    println!(
        "  {} {}",
        console::style("⎿").dim(),
        console::style(format!(
            "gate {} · {:.1}s",
            if passed { "passed" } else { "failed" },
            elapsed_ms as f64 / 1000.0
        ))
        .dim()
    );
    ledger::append(&ledger::PhaseRecord {
        ts_ms: ledger::now_ms(),
        session_id: meta.session_id.to_string(),
        run_id: meta.run_id.to_string(),
        phase: "gate".to_string(),
        cycle,
        role: "gate".to_string(),
        provider: String::new(),
        config_model: String::new(),
        reported_model: None,
        context_limit: None,
        input_tokens: None,
        output_tokens: None,
        duration_ms: elapsed_ms,
        verdict: Some(verdict),
        task_preview: safe_truncate(meta.task, 120),
        permission_policy: None,
        permission_denials: None,
        plan_exemplars_injected: None,
        plan_exemplar_run_ids: None,
        review_exemplars_injected: None,
        review_exemplar_run_ids: None,
        playbook_injected: None,
        arena_rank: None,
        arena_winner: None,
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    fn write_package_json(dir: &std::path::Path, scripts: &str) {
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"scripts":{scripts}}}"#),
        )
        .expect("write package.json");
    }

    #[test]
    fn detects_js_package_manager_from_lockfiles() {
        for (lockfile, expected) in [
            ("pnpm-lock.yaml", "pnpm"),
            ("yarn.lock", "yarn"),
            ("package-lock.json", "npm"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            fs::write(temp.path().join(lockfile), "").expect("write lockfile");
            assert_eq!(super::detect_js_package_manager(temp.path()).0, expected);
        }

        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(super::detect_js_package_manager(temp.path()).0, "npm");
    }

    #[test]
    fn derives_only_existing_test_and_build_scripts() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(temp.path(), r#"{"build":"next build","lint":"eslint ."}"#);

        let resolved = super::resolve_gates(temp.path(), None, Vec::new());

        assert_eq!(resolved.gates, vec!["npm run build"]);
        assert!(matches!(
            resolved.source,
            super::GateSource::Derived {
                manifest: "package.json",
                ..
            }
        ));
    }

    #[test]
    fn excludes_placeholder_test_and_lint_scripts() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(
            temp.path(),
            r#"{"test":"echo \"Error: no test specified\" && exit 1","lint":"eslint ."}"#,
        );

        let resolved =
            super::resolve_gates(temp.path(), None, vec!["fallback command".to_string()]);

        assert_eq!(resolved.gates, vec!["fallback command"]);
        assert_eq!(resolved.source, super::GateSource::Global);
    }

    #[test]
    fn local_file_takes_priority_over_derived_gates() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(temp.path(), r#"{"test":"vitest"}"#);
        fs::write(
            temp.path().join(super::LOCAL_GATES_FILE),
            "- custom verify\n",
        )
        .expect("write local gates");

        let resolved = super::resolve_gates(temp.path(), None, vec!["global verify".to_string()]);

        assert_eq!(resolved.gates, vec!["custom verify"]);
        assert!(matches!(resolved.source, super::GateSource::LocalFile(_)));
    }

    #[test]
    fn uncommitted_local_file_in_fallback_dir_takes_priority() {
        let implementation = tempfile::tempdir().expect("implementation tempdir");
        let original = tempfile::tempdir().expect("original tempdir");
        write_package_json(implementation.path(), r#"{"test":"vitest"}"#);
        fs::write(
            original.path().join(super::LOCAL_GATES_FILE),
            "- original verify\n",
        )
        .expect("write local gates");

        let resolved = super::resolve_gates(
            implementation.path(),
            Some(original.path()),
            vec!["global verify".to_string()],
        );

        assert_eq!(resolved.gates, vec!["original verify"]);
        assert_eq!(
            resolved.source,
            super::GateSource::LocalFile(original.path().join(super::LOCAL_GATES_FILE))
        );
    }

    #[test]
    fn local_empty_list_is_explicit_opt_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(temp.path(), r#"{"test":"vitest"}"#);
        fs::write(temp.path().join(super::LOCAL_GATES_FILE), "[]\n").expect("write local gates");

        let resolved = super::resolve_gates(temp.path(), None, vec!["global verify".to_string()]);

        assert!(resolved.gates.is_empty());
        assert!(matches!(resolved.source, super::GateSource::LocalFile(_)));
    }

    #[test]
    fn cargo_repo_preserves_global_gate_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Cargo.toml"), "[workspace]\n").expect("write manifest");

        let global = vec![
            " cargo fmt --check ".to_string(),
            "cargo test -p goose-cli".to_string(),
        ];
        let resolved = super::resolve_gates(temp.path(), None, global);

        assert_eq!(resolved.source, super::GateSource::Global);
        assert_eq!(
            resolved.gates,
            vec!["cargo fmt --check", "cargo test -p goose-cli"]
        );
    }

    #[test]
    fn pyproject_repo_uses_global_gate_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\n",
        )
        .expect("write manifest");

        let resolved = super::resolve_gates(temp.path(), None, vec!["uv run pytest".to_string()]);

        assert_eq!(resolved.source, super::GateSource::Global);
        assert_eq!(resolved.gates, vec!["uv run pytest"]);
    }

    #[test]
    fn derives_go_build_and_test() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("go.mod"), "module example.com/test\n").expect("write manifest");

        let resolved = super::resolve_gates(temp.path(), None, Vec::new());

        assert_eq!(resolved.gates, vec!["go build ./...", "go test ./..."]);
    }

    #[test]
    fn invalid_local_file_warns_and_falls_through_to_derivation() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(temp.path(), r#"{"test":"vitest"}"#);
        fs::write(temp.path().join(super::LOCAL_GATES_FILE), "command: test\n")
            .expect("write local gates");

        let resolved = super::resolve_gates(temp.path(), None, Vec::new());

        assert_eq!(resolved.gates, vec!["npm run test"]);
        assert!(resolved.warning.is_some());
    }

    #[test]
    fn partition_skips_build_tool_gates_without_manifests() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gates = vec![
            "cargo fmt --check".to_string(),
            "cargo test -p goose-cli".to_string(),
            "pnpm test".to_string(),
            "go vet ./...".to_string(),
        ];

        let partition = super::partition_gates(temp.path(), &gates);

        assert!(
            partition.applicable.is_empty(),
            "no manifest present, so no build-tool gate should run: {:?}",
            partition.applicable
        );
        assert_eq!(partition.skipped.len(), 4);
        assert!(partition
            .skipped
            .iter()
            .any(|s| s.reason.contains("Cargo.toml")));
        assert!(partition
            .skipped
            .iter()
            .any(|s| s.reason.contains("package.json")));
    }

    #[test]
    fn partition_keeps_cargo_gate_when_cargo_toml_present() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Cargo.toml"), "[package]\n").expect("write manifest");

        let partition = super::partition_gates(temp.path(), &["cargo fmt --check".to_string()]);

        // cargo is on PATH in the test env, and the manifest now exists.
        assert_eq!(partition.applicable, vec!["cargo fmt --check".to_string()]);
        assert!(partition.skipped.is_empty());
    }

    #[test]
    fn partition_keeps_shell_form_gates() {
        let temp = tempfile::tempdir().expect("tempdir");

        let partition = super::partition_gates(
            temp.path(),
            &["test -f package.json && pnpm test".to_string()],
        );

        assert_eq!(partition.applicable.len(), 1);
        assert!(partition.skipped.is_empty());
    }

    #[test]
    fn partition_skips_uninstalled_tool() {
        let temp = tempfile::tempdir().expect("tempdir");

        let partition = super::partition_gates(
            temp.path(),
            &["goose-nonexistent-gate-tool-xyz --check".to_string()],
        );

        assert!(partition.applicable.is_empty());
        assert_eq!(partition.skipped.len(), 1);
        assert!(partition.skipped[0].reason.contains("not installed"));
    }

    #[tokio::test]
    async fn run_gates_passes_when_all_commands_succeed() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert!(matches!(
            super::run_gates(temp.path(), &["true".to_string()]).await,
            super::GateOutcome::Passed { .. }
        ));
        assert!(matches!(
            super::run_gates(temp.path(), &[]).await,
            super::GateOutcome::Passed { .. }
        ));
    }

    #[tokio::test]
    async fn run_gates_captures_passing_output_for_review() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(temp.path(), &["echo passing-gate-output".to_string()]).await {
            super::GateOutcome::Passed { runs } => {
                assert_eq!(runs.len(), 1);
                assert_eq!(runs[0].command, "echo passing-gate-output");
                assert!(runs[0].output_tail.contains("passing-gate-output"));
                let section = super::gate_outputs_review_section(&runs, 40);
                assert!(section.contains("$ echo passing-gate-output"));
                assert!(section.contains("passing-gate-output"));
                assert!(super::gate_outputs_review_section(&[], 40).is_empty());
            }
            super::GateOutcome::Failed { .. } => panic!("expected passing gate"),
        }
    }

    #[tokio::test]
    async fn run_gates_uses_impl_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("sentinel"), "present\n").expect("write sentinel");

        assert!(matches!(
            super::run_gates(temp.path(), &["test -f sentinel".to_string()]).await,
            super::GateOutcome::Passed { .. }
        ));
    }

    #[tokio::test]
    async fn run_gates_stops_at_first_failing_command() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(
            temp.path(),
            &["true".to_string(), "false".to_string(), "true".to_string()],
        )
        .await
        {
            super::GateOutcome::Failed { command, .. } => assert_eq!(command, "false"),
            super::GateOutcome::Passed { .. } => panic!("expected failing gate"),
        }
    }

    #[tokio::test]
    async fn run_gates_captures_stderr_tail_via_shell() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(temp.path(), &["echo GATE_MARKER 1>&2; exit 1".to_string()]).await {
            super::GateOutcome::Failed { output_tail, .. } => {
                assert!(output_tail.contains("GATE_MARKER"), "{output_tail}");
            }
            super::GateOutcome::Passed { .. } => panic!("expected failing gate"),
        }
    }

    #[test]
    fn command_needs_shell_detects_operators() {
        assert!(!super::command_needs_shell(
            "cargo clippy --all-targets -- -D warnings"
        ));
        assert!(!super::command_needs_shell("cargo fmt --check"));
        assert!(super::command_needs_shell("a && b"));
        assert!(super::command_needs_shell("a | b"));
        assert!(super::command_needs_shell("a > f"));
    }

    #[test]
    fn tail_truncate_keeps_tail() {
        assert_eq!(super::tail_truncate("abcdef", 20), "abcdef");
        assert_eq!(super::tail_truncate("abcdef", 5), "...ef");
        assert_eq!(super::tail_truncate("abcdefgh", 6), "...fgh");
    }

    #[test]
    fn next_gate_step_reimplements_then_aborts() {
        let mut gate_retries = 0;

        for _ in 0..2 {
            match super::next_gate_step(
                super::GateOutcome::Failed {
                    command: "false".to_string(),
                    output_tail: "tail text".to_string(),
                },
                &mut gate_retries,
                2,
            ) {
                super::GateStep::Reimplement(instruction) => {
                    assert!(instruction.contains("false"));
                    assert!(instruction.contains("tail text"));
                }
                other => panic!("expected reimplement, got {other:?}"),
            }
        }

        match super::next_gate_step(
            super::GateOutcome::Failed {
                command: "false".to_string(),
                output_tail: "tail text".to_string(),
            },
            &mut gate_retries,
            2,
        ) {
            super::GateStep::Abort(reason) => {
                assert!(reason.contains("false"));
                assert!(reason.contains("2"));
            }
            other => panic!("expected abort, got {other:?}"),
        }
        assert_eq!(gate_retries, 2);
    }

    #[test]
    fn next_gate_step_proceeds_on_pass() {
        let mut gate_retries = 0;

        assert!(matches!(
            super::next_gate_step(
                super::GateOutcome::Passed { runs: Vec::new() },
                &mut gate_retries,
                2
            ),
            super::GateStep::Proceed
        ));
        assert_eq!(gate_retries, 0);
    }

    #[tokio::test]
    async fn gates_unset_is_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gates = Vec::new();

        assert!(matches!(
            super::run_gates(temp.path(), &gates).await,
            super::GateOutcome::Passed { .. }
        ));
        assert_eq!(super::gate_passed_review_note(&gates), "");
    }

    #[test]
    fn gate_passed_review_note_lists_commands() {
        let gates = vec![
            "cargo fmt --check".to_string(),
            "cargo test -p goose-cli".to_string(),
        ];

        assert_eq!(
            super::gate_passed_review_note(&gates),
            "\n\ngates passed: cargo fmt --check; cargo test -p goose-cli"
        );
    }

    #[tokio::test]
    async fn run_gates_times_out_and_marks_failure() {
        let _guard = env_lock::lock_env([
            ("GOOSE_ORCH_GATE_TIMEOUT_SECS", Some("1")),
            ("GOOSE_ORCH_GATE_ENV", Some("inherit")),
        ]);
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(temp.path(), &["sleep 30".to_string()]).await {
            super::GateOutcome::Failed { output_tail, .. } => {
                assert!(output_tail.contains("timed out"), "{output_tail}");
                // The timeout message names the knob to raise.
                assert!(
                    output_tail.contains("GOOSE_ORCH_GATE_TIMEOUT_SECS"),
                    "{output_tail}"
                );
            }
            super::GateOutcome::Passed { .. } => panic!("expected timeout failure"),
        }
    }

    #[tokio::test]
    async fn run_gates_failure_under_scrub_appends_env_hint() {
        let _guard = env_lock::lock_env([
            ("GOOSE_ORCH_GATE_ENV", Some("scrub")),
            ("GOOSE_ORCH_GATE_TIMEOUT_SECS", Some("60")),
        ]);
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(temp.path(), &["false".to_string()]).await {
            super::GateOutcome::Failed { output_tail, .. } => {
                assert!(
                    output_tail.contains("gate env is scrubbed by default"),
                    "{output_tail}"
                );
                assert!(
                    output_tail.contains("GOOSE_ORCH_GATE_ENV=inherit"),
                    "{output_tail}"
                );
            }
            super::GateOutcome::Passed { .. } => panic!("expected failing gate"),
        }
    }

    #[tokio::test]
    async fn run_gates_scrub_drops_secret_env_but_keeps_path() {
        let _guard = env_lock::lock_env([
            ("GOOSE_ORCH_GATE_ENV", Some("scrub")),
            ("GOOSE_ORCH_GATE_TIMEOUT_SECS", Some("60")),
            ("FOO_API_KEY", Some("supersecret")),
        ]);
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(
            temp.path(),
            &["printf 'KEY=[%s] PATH=[%s]' \"$FOO_API_KEY\" \"$PATH\"".to_string()],
        )
        .await
        {
            super::GateOutcome::Passed { runs } => {
                let output = &runs[0].output_tail;
                assert!(
                    output.contains("KEY=[]"),
                    "secret should be scrubbed: {output}"
                );
                assert!(!output.contains("supersecret"), "{output}");
                assert!(!output.contains("PATH=[]"), "PATH should survive: {output}");
            }
            super::GateOutcome::Failed { output_tail, .. } => {
                panic!("expected pass, got failure: {output_tail}")
            }
        }
    }

    #[tokio::test]
    async fn run_gates_inherit_passes_secret_env_through() {
        let _guard = env_lock::lock_env([
            ("GOOSE_ORCH_GATE_ENV", Some("inherit")),
            ("GOOSE_ORCH_GATE_TIMEOUT_SECS", Some("60")),
            ("BAR_API_KEY", Some("inheritedsecret")),
        ]);
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(
            temp.path(),
            &["printf 'KEY=[%s]' \"$BAR_API_KEY\"".to_string()],
        )
        .await
        {
            super::GateOutcome::Passed { runs } => {
                assert!(
                    runs[0].output_tail.contains("inheritedsecret"),
                    "{}",
                    runs[0].output_tail
                );
            }
            super::GateOutcome::Failed { output_tail, .. } => {
                panic!("expected pass, got failure: {output_tail}")
            }
        }
    }

    #[test]
    fn is_secret_env_key_matches_credential_patterns_only() {
        assert!(super::is_secret_env_key("FOO_API_KEY"));
        assert!(super::is_secret_env_key("GITHUB_TOKEN"));
        assert!(super::is_secret_env_key("MY_SECRET"));
        assert!(super::is_secret_env_key("ANTHROPIC_BASE_URL"));
        assert!(super::is_secret_env_key("AWS_ACCESS_KEY_ID"));
        assert!(!super::is_secret_env_key("PATH"));
        assert!(!super::is_secret_env_key("HOME"));
        assert!(!super::is_secret_env_key("CARGO_HOME"));
        assert!(!super::is_secret_env_key("CI"));
    }

    #[test]
    fn seed_allowed_commands_includes_git_and_detected_tools() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Cargo.toml"), "[package]\n").expect("manifest");

        let resolved =
            super::resolve_gates(temp.path(), None, vec!["cargo test -p goose".to_string()]);
        let seed = super::seed_allowed_commands(temp.path(), &resolved);

        assert!(seed.contains(&"git".to_string()));
        assert!(seed.contains(&"cargo".to_string()));
        // Sorted and de-duplicated.
        let mut sorted = seed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(seed, sorted);
    }

    #[test]
    fn derived_gates_notice_only_for_derived_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_package_json(temp.path(), r#"{"test":"vitest"}"#);
        let derived = super::resolve_gates(temp.path(), None, Vec::new());
        assert!(super::derived_gates_notice(&derived)
            .expect("derived notice")
            .contains("package.json"));

        let cargo = tempfile::tempdir().expect("tempdir");
        fs::write(cargo.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
        let global = super::resolve_gates(cargo.path(), None, vec!["cargo test".to_string()]);
        assert!(super::derived_gates_notice(&global).is_none());
    }
}
