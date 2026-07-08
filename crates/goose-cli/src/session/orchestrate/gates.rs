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

    cmd.current_dir(impl_dir).output()
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
}
