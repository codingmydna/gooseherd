use goose::config::paths::Paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

/// One orchestration phase (plan / implement / review / arena) as recorded in
/// the persistent run ledger, `<state_dir>/orch_ledger.jsonl`.
#[derive(Serialize, Deserialize, Clone)]
pub struct PhaseRecord {
    pub ts_ms: u128,
    pub session_id: String,
    pub run_id: String,
    pub phase: String,
    pub cycle: u32,
    pub role: String,
    pub provider: String,
    pub config_model: String,
    /// Model name the provider reported back for this call — differs from
    /// `config_model` when the backend silently substitutes a model.
    pub reported_model: Option<String>,
    /// Context limit the provider session advertises; a model fingerprint
    /// even when `reported_model` is generic (e.g. "default").
    pub context_limit: Option<usize>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub duration_ms: u64,
    pub verdict: Option<String>,
    pub permission_policy: Option<String>,
    pub permission_denials: Option<u64>,
    pub task_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_exemplars_injected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_exemplar_run_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_exemplars_injected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_exemplar_run_ids: Option<Vec<String>>,
    /// Whether the Fable playbook was injected into this role's system prompt —
    /// part of the uplift on/off split in `/stats`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playbook_injected: Option<bool>,
    /// Arena-only: 1-based finishing position assigned by the judge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arena_rank: Option<u32>,
    /// Arena-only: whether this contestant won its arena.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arena_winner: Option<bool>,
}

fn ledger_path() -> PathBuf {
    Paths::state_dir().join("orch_ledger.jsonl")
}

pub fn path_display() -> String {
    ledger_path().display().to_string()
}

pub fn append(record: &PhaseRecord) {
    let Ok(json) = serde_json::to_string(record) else {
        return;
    };
    let path = ledger_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{}", json);
    }
}

pub fn read_all() -> Vec<PhaseRecord> {
    let Ok(content) = std::fs::read_to_string(ledger_path()) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Runs and outcomes for one implementer model in one uplift bucket
/// (with-uplift vs without-uplift).
#[derive(Default, Clone)]
pub struct UpliftBucket {
    pub runs: u32,
    pub approved: u32,
    cycles_to_approval_sum: u32,
}

impl UpliftBucket {
    pub fn approval_rate(&self) -> Option<f64> {
        (self.runs > 0).then(|| self.approved as f64 / self.runs as f64)
    }

    pub fn mean_cycles_to_approval(&self) -> Option<f64> {
        (self.approved > 0).then(|| self.cycles_to_approval_sum as f64 / self.approved as f64)
    }
}

/// Per-implementer-model uplift measurement, split by whether any uplift
/// (exemplar or playbook) was injected during the run.
#[derive(Clone)]
pub struct UpliftModelStats {
    pub model: String,
    pub with_uplift: UpliftBucket,
    pub without_uplift: UpliftBucket,
}

/// Aggregate the ledger into per-model uplift stats. Each run is attributed to
/// the model that ran its `implement` phase, bucketed by whether any uplift
/// injection (plan/review exemplars or the playbook) fired during the run, and
/// scored on approval rate and mean cycles-to-approval. Pure over its input so
/// it is unit-testable on synthetic rows.
pub fn aggregate_uplift(records: &[PhaseRecord]) -> Vec<UpliftModelStats> {
    let mut runs: BTreeMap<&str, Vec<&PhaseRecord>> = BTreeMap::new();
    for record in records {
        runs.entry(record.run_id.as_str()).or_default().push(record);
    }

    let mut by_model: BTreeMap<String, UpliftModelStats> = BTreeMap::new();
    for rows in runs.values() {
        let Some(implementer) = rows.iter().rev().find(|row| row.phase == "implement") else {
            continue;
        };
        let model = format!("{}/{}", implementer.provider, implementer.config_model);
        let uplift_on = rows.iter().any(|row| {
            row.plan_exemplars_injected == Some(true)
                || row.review_exemplars_injected == Some(true)
                || row.playbook_injected == Some(true)
        });
        let approved_cycle = rows
            .iter()
            .filter(|row| row.verdict.as_deref() == Some("APPROVED"))
            .map(|row| row.cycle)
            .min();

        let stats = by_model
            .entry(model.clone())
            .or_insert_with(|| UpliftModelStats {
                model,
                with_uplift: UpliftBucket::default(),
                without_uplift: UpliftBucket::default(),
            });
        let bucket = if uplift_on {
            &mut stats.with_uplift
        } else {
            &mut stats.without_uplift
        };
        bucket.runs += 1;
        if let Some(cycle) = approved_cycle {
            bucket.approved += 1;
            bucket.cycles_to_approval_sum += cycle;
        }
    }

    by_model.into_values().collect()
}

/// Arena wins per model — how many times each model was the judged winner.
pub fn aggregate_arena_wins(records: &[PhaseRecord]) -> Vec<(String, u32)> {
    let mut wins: BTreeMap<String, u32> = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.phase == "arena" && record.arena_winner == Some(true))
    {
        *wins
            .entry(format!("{}/{}", record.provider, record.config_model))
            .or_default() += 1;
    }
    wins.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(run_id: &str, phase: &str, provider: &str, model: &str) -> PhaseRecord {
        PhaseRecord {
            ts_ms: 0,
            session_id: "s".to_string(),
            run_id: run_id.to_string(),
            phase: phase.to_string(),
            cycle: 0,
            role: phase.to_string(),
            provider: provider.to_string(),
            config_model: model.to_string(),
            reported_model: None,
            context_limit: None,
            input_tokens: None,
            output_tokens: None,
            duration_ms: 0,
            verdict: None,
            permission_policy: None,
            permission_denials: None,
            task_preview: String::new(),
            plan_exemplars_injected: None,
            plan_exemplar_run_ids: None,
            review_exemplars_injected: None,
            review_exemplar_run_ids: None,
            playbook_injected: None,
            arena_rank: None,
            arena_winner: None,
        }
    }

    fn implement(run_id: &str, provider: &str, model: &str) -> PhaseRecord {
        row(run_id, "implement", provider, model)
    }

    fn review(run_id: &str, cycle: u32, verdict: &str) -> PhaseRecord {
        let mut r = row(run_id, "review", "codex-acp", "gpt-5.5");
        r.cycle = cycle;
        r.verdict = Some(verdict.to_string());
        r
    }

    #[test]
    fn legacy_rows_without_new_fields_still_parse() {
        // A ledger line written before playbook/arena fields existed.
        let line = r#"{"ts_ms":1,"session_id":"s","run_id":"r","phase":"review","cycle":1,"role":"reviewer","provider":"codex-acp","config_model":"gpt-5.5","reported_model":null,"context_limit":null,"input_tokens":null,"output_tokens":null,"duration_ms":0,"verdict":"APPROVED","permission_policy":null,"permission_denials":null,"task_preview":"t"}"#;
        let record: PhaseRecord = serde_json::from_str(line).expect("parse legacy row");
        assert_eq!(record.playbook_injected, None);
        assert_eq!(record.arena_winner, None);
    }

    #[test]
    fn aggregate_uplift_splits_by_injection_and_scores_approval() {
        let mut records = Vec::new();

        // Run A: gpt-5.5 implementer, exemplar injected, approved on cycle 2.
        let mut plan_a = row("A", "plan", "fable", "fable-5");
        plan_a.plan_exemplars_injected = Some(true);
        records.push(plan_a);
        records.push(implement("A", "codex-acp", "gpt-5.5"));
        records.push(review("A", 1, "REVISE"));
        records.push(review("A", 2, "APPROVED"));

        // Run B: gpt-5.5 implementer, no injection, never approved.
        records.push(implement("B", "codex-acp", "gpt-5.5"));
        records.push(review("B", 1, "REVISE"));

        // Run C: gpt-5.5 implementer, playbook injected, approved on cycle 1.
        let mut plan_c = row("C", "plan", "fable", "fable-5");
        plan_c.playbook_injected = Some(true);
        records.push(plan_c);
        records.push(implement("C", "codex-acp", "gpt-5.5"));
        records.push(review("C", 1, "APPROVED"));

        let stats = aggregate_uplift(&records);
        assert_eq!(stats.len(), 1);
        let gpt = &stats[0];
        assert_eq!(gpt.model, "codex-acp/gpt-5.5");

        // With uplift: runs A and C, both approved (cycles 2 and 1 → mean 1.5).
        assert_eq!(gpt.with_uplift.runs, 2);
        assert_eq!(gpt.with_uplift.approved, 2);
        assert_eq!(gpt.with_uplift.approval_rate(), Some(1.0));
        assert_eq!(gpt.with_uplift.mean_cycles_to_approval(), Some(1.5));

        // Without uplift: run B, never approved.
        assert_eq!(gpt.without_uplift.runs, 1);
        assert_eq!(gpt.without_uplift.approved, 0);
        assert_eq!(gpt.without_uplift.approval_rate(), Some(0.0));
        assert_eq!(gpt.without_uplift.mean_cycles_to_approval(), None);
    }

    #[test]
    fn aggregate_uplift_ignores_runs_without_an_implement_phase() {
        let records = vec![row("only-plan", "plan", "fable", "fable-5")];
        assert!(aggregate_uplift(&records).is_empty());
    }

    #[test]
    fn aggregate_arena_wins_counts_winners_per_model() {
        let mut records = Vec::new();
        let mut winner = row("arena-1", "arena", "codex-acp", "gpt-5.5");
        winner.arena_winner = Some(true);
        winner.arena_rank = Some(1);
        records.push(winner);
        let mut loser = row("arena-1", "arena", "claude-acp", "opus");
        loser.arena_winner = Some(false);
        loser.arena_rank = Some(2);
        records.push(loser);
        let mut winner2 = row("arena-2", "arena", "codex-acp", "gpt-5.5");
        winner2.arena_winner = Some(true);
        records.push(winner2);

        let wins = aggregate_arena_wins(&records);
        assert_eq!(wins, vec![("codex-acp/gpt-5.5".to_string(), 2)]);
    }
}
