use crate::session::ledger;
use goose::utils::safe_truncate;
use std::path::Path;

use super::phases::PhaseMeta;

const GATE_OUTPUT_TAIL_LIMIT: usize = 4_000;

#[derive(Debug)]
pub(super) enum GateOutcome {
    Passed,
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateOrigin {
    LocalFile,
    Derived(String),
    Global,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResolvedGates {
    pub gates: Vec<String>,
    pub origin: GateOrigin,
}

pub(crate) fn resolve_gates(repo_dir: &Path, global_gates: Vec<String>) -> ResolvedGates {
    if let Ok(contents) = std::fs::read_to_string(repo_dir.join(".goosegates")) {
        let gates = parse_gate_lines(&contents);
        if !gates.is_empty() {
            return ResolvedGates {
                gates,
                origin: GateOrigin::LocalFile,
            };
        }
    }

    // Cargo repositories retain their existing global configuration unchanged.
    if !repo_dir.join("Cargo.toml").exists() {
        let (gates, sources) = derive_manifest_gates(repo_dir);
        if !gates.is_empty() {
            return ResolvedGates {
                gates,
                origin: GateOrigin::Derived(sources.join("+")),
            };
        }
    }

    let gates = effective_gates(global_gates);
    if gates.is_empty() {
        ResolvedGates {
            gates,
            origin: GateOrigin::None,
        }
    } else {
        ResolvedGates {
            gates,
            origin: GateOrigin::Global,
        }
    }
}

fn parse_gate_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToString::to_string)
        .collect()
}

fn derive_manifest_gates(repo_dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut gates = Vec::new();
    let mut sources = Vec::new();

    if let Ok(package_json) = std::fs::read_to_string(repo_dir.join("package.json")) {
        let package_manager = node_package_manager(repo_dir);
        let node_gates = derive_node_gates(&package_json, package_manager);
        if !node_gates.is_empty() {
            gates.extend(node_gates);
            sources.push(format!("package.json ({package_manager})"));
        }
    }
    if repo_dir.join("go.mod").exists() {
        gates.extend(["go build ./...".to_string(), "go test ./...".to_string()]);
        sources.push("go.mod".to_string());
    }
    if let Ok(pyproject) = std::fs::read_to_string(repo_dir.join("pyproject.toml")) {
        let python_gates = derive_python_gates(&pyproject, repo_dir.join("uv.lock").exists());
        if !python_gates.is_empty() {
            gates.extend(python_gates);
            sources.push("pyproject.toml".to_string());
        }
    }
    (gates, sources)
}

fn node_package_manager(repo_dir: &Path) -> &'static str {
    for (lockfile, package_manager) in [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("package-lock.json", "npm"),
    ] {
        if repo_dir.join(lockfile).exists() {
            return package_manager;
        }
    }
    "npm"
}

fn derive_node_gates(package_json: &str, package_manager: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return Vec::new();
    };
    let Some(scripts) = value.get("scripts").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };

    ["test", "build"]
        .into_iter()
        .filter_map(|name| {
            let script = scripts.get(name)?.as_str()?.trim();
            if script.is_empty() || (name == "test" && is_placeholder_test_script(script)) {
                None
            } else {
                Some(format!("{package_manager} run {name}"))
            }
        })
        .collect()
}

fn is_placeholder_test_script(script: &str) -> bool {
    script.to_ascii_lowercase().contains("no test specified")
}

fn derive_python_gates(pyproject: &str, has_uv_lock: bool) -> Vec<String> {
    // uv.lock makes this safe on a clean checkout; other Python setups may lack an environment.
    if has_uv_lock && pyproject.contains("pytest") {
        vec!["uv run pytest".to_string()]
    } else {
        Vec::new()
    }
}

#[derive(Debug)]
pub(crate) struct GateSkip {
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub(crate) struct GatePartition {
    pub applicable: Vec<String>,
    pub skipped: Vec<GateSkip>,
}

/// Split gates into those that can run against `impl_dir` and those that must be
/// skipped. A gate is skipped when its build tool's manifest is absent from the
/// target repo (a `cargo` gate in a repo with no `Cargo.toml`) or the tool is
/// not installed. Without this, an unrelated gate fails forever and re-dispatches
/// the implementer — which is how orchestrating a JS repo with goose's own Rust
/// gates looped and pushed the implementer to fabricate a `Cargo.toml`.
pub(crate) fn partition_gates(impl_dir: &Path, gates: &[String]) -> GatePartition {
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

pub(super) fn run_gates(impl_dir: &Path, gates: &[String]) -> GateOutcome {
    for command in gates {
        if command.trim().is_empty() {
            continue;
        }
        let output = match spawn_gate(impl_dir, command) {
            Ok(output) => output,
            Err(error) => {
                return GateOutcome::Failed {
                    command: command.clone(),
                    output_tail: format!("failed to launch gate command: {error}"),
                };
            }
        };

        if !output.status.success() {
            let mut combined = format!("status: {}\n", output.status);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stdout.trim().is_empty() {
                combined.push_str(&format!("stdout:\n{stdout}\n"));
            }
            if !stderr.trim().is_empty() {
                combined.push_str(&format!("stderr:\n{stderr}\n"));
            }
            return GateOutcome::Failed {
                command: command.clone(),
                output_tail: tail_truncate(&combined, GATE_OUTPUT_TAIL_LIMIT),
            };
        }
    }

    GateOutcome::Passed
}

fn spawn_gate(impl_dir: &Path, command: &str) -> std::io::Result<std::process::Output> {
    use std::process::Command;

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

    cmd.current_dir(impl_dir).env("CI", "true").output()
}

pub(crate) fn gate_origin_banner(resolved: &ResolvedGates, partition: &GatePartition) -> String {
    let skipped = if partition.skipped.is_empty() {
        String::new()
    } else {
        format!(
            " ({} of {} skipped)",
            partition.skipped.len(),
            resolved.gates.len()
        )
    };
    match &resolved.origin {
        GateOrigin::LocalFile => format!(
            "gates: {} from .goosegates{skipped}",
            partition.applicable.len()
        ),
        GateOrigin::Derived(source) => {
            format!(
                "gates: {} derived from {source}{skipped}",
                partition.applicable.len()
            )
        }
        GateOrigin::Global => format!(
            "gates: {} from global GOOSE_ORCH_GATES{skipped}",
            partition.applicable.len()
        ),
        GateOrigin::None => {
            "gates: none (no .goosegates, no supported manifest, GOOSE_ORCH_GATES unset)"
                .to_string()
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
        GateOutcome::Passed => GateStep::Proceed,
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
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

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

    #[test]
    fn run_gates_passes_when_all_commands_succeed() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert!(matches!(
            super::run_gates(temp.path(), &["true".to_string()]),
            super::GateOutcome::Passed
        ));
        assert!(matches!(
            super::run_gates(temp.path(), &[]),
            super::GateOutcome::Passed
        ));
    }

    #[test]
    fn run_gates_uses_impl_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("sentinel"), "present\n").expect("write sentinel");

        assert!(matches!(
            super::run_gates(temp.path(), &["test -f sentinel".to_string()]),
            super::GateOutcome::Passed
        ));
    }

    #[test]
    fn run_gates_stops_at_first_failing_command() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(
            temp.path(),
            &["true".to_string(), "false".to_string(), "true".to_string()],
        ) {
            super::GateOutcome::Failed { command, .. } => assert_eq!(command, "false"),
            super::GateOutcome::Passed => panic!("expected failing gate"),
        }
    }

    #[test]
    fn run_gates_captures_stderr_tail_via_shell() {
        let temp = tempfile::tempdir().expect("tempdir");

        match super::run_gates(temp.path(), &["echo GATE_MARKER 1>&2; exit 1".to_string()]) {
            super::GateOutcome::Failed { output_tail, .. } => {
                assert!(output_tail.contains("GATE_MARKER"), "{output_tail}");
            }
            super::GateOutcome::Passed => panic!("expected failing gate"),
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
            super::next_gate_step(super::GateOutcome::Passed, &mut gate_retries, 2),
            super::GateStep::Proceed
        ));
        assert_eq!(gate_retries, 0);
    }

    #[test]
    fn gates_unset_is_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gates = Vec::new();

        assert!(matches!(
            super::run_gates(temp.path(), &gates),
            super::GateOutcome::Passed
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

    #[test]
    fn node_package_manager_prefers_lockfiles_in_priority_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(super::node_package_manager(temp.path()), "npm");
        fs::write(temp.path().join("package-lock.json"), "{}").expect("npm lock");
        assert_eq!(super::node_package_manager(temp.path()), "npm");
        fs::write(temp.path().join("yarn.lock"), "").expect("yarn lock");
        assert_eq!(super::node_package_manager(temp.path()), "yarn");
        fs::write(temp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'").expect("pnpm lock");
        assert_eq!(super::node_package_manager(temp.path()), "pnpm");
    }

    #[test]
    fn node_derivation_uses_only_real_test_and_build_scripts() {
        let gates = super::derive_node_gates(
            r#"{"scripts":{"test":"vitest run","build":"tsc","lint":"eslint .","typecheck":"tsc --noEmit"}}"#,
            "pnpm",
        );
        assert_eq!(gates, ["pnpm run test", "pnpm run build"]);

        let placeholder = super::derive_node_gates(
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1","lint":"eslint ."}}"#,
            "npm",
        );
        assert!(placeholder.is_empty());
    }

    #[test]
    fn local_gate_file_takes_precedence_over_manifest_derivation() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join(".goosegates"),
            "# checks\ncustom test\n\ncustom build\n",
        )
        .expect("gate file");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run","build":"tsc"}}"#,
        )
        .expect("package json");

        let resolved = super::resolve_gates(temp.path(), vec!["cargo test".to_string()]);
        assert_eq!(resolved.origin, super::GateOrigin::LocalFile);
        assert_eq!(resolved.gates, ["custom test", "custom build"]);
    }

    #[test]
    fn resolve_gates_falls_back_to_global_and_preserves_cargo_repos() {
        let temp = tempfile::tempdir().expect("tempdir");
        let global = vec!["cargo fmt --check".to_string()];
        let resolved = super::resolve_gates(temp.path(), global.clone());
        assert_eq!(resolved.origin, super::GateOrigin::Global);

        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .expect("cargo manifest");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .expect("package json");
        let resolved = super::resolve_gates(temp.path(), global);
        assert_eq!(resolved.origin, super::GateOrigin::Global);
        assert_eq!(resolved.gates, ["cargo fmt --check"]);

        let empty = tempfile::tempdir().expect("empty tempdir");
        assert_eq!(
            super::resolve_gates(empty.path(), Vec::new()).origin,
            super::GateOrigin::None
        );
    }

    #[test]
    fn derives_go_and_safe_uv_pytest_gates() {
        let go = tempfile::tempdir().expect("go tempdir");
        fs::write(go.path().join("go.mod"), "module example.com/test\n").expect("go mod");
        let resolved = super::resolve_gates(go.path(), Vec::new());
        assert_eq!(resolved.gates, ["go build ./...", "go test ./..."]);

        let python = tempfile::tempdir().expect("python tempdir");
        fs::write(
            python.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"pytest\"]\n",
        )
        .expect("pyproject");
        fs::write(python.path().join("uv.lock"), "version = 1\n").expect("uv lock");
        let resolved = super::resolve_gates(python.path(), Vec::new());
        assert_eq!(resolved.gates, ["uv run pytest"]);
        assert!(super::derive_python_gates("pytest", false).is_empty());
    }

    #[test]
    fn gate_origin_banner_reports_source_and_skips() {
        let resolved = super::ResolvedGates {
            gates: vec!["npm run test".to_string()],
            origin: super::GateOrigin::Derived("package.json".to_string()),
        };
        let partition = super::GatePartition {
            applicable: vec!["npm run test".to_string()],
            skipped: Vec::new(),
        };
        assert_eq!(
            super::gate_origin_banner(&resolved, &partition),
            "gates: 1 derived from package.json"
        );
    }
}
