use goose::config::paths::Paths;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// One orchestration phase (plan / implement / review) as recorded in the
/// persistent run ledger, `<state_dir>/orch_ledger.jsonl`.
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
