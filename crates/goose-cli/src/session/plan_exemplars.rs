use goose::config::{paths::Paths, Config};
use goose::utils::safe_truncate;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXEMPLARS_DIR: &str = "plan_exemplars";
const INDEX_FILE: &str = "exemplars.jsonl";
const ENABLED_KEY: &str = "GOOSE_PLAN_EXEMPLARS";
const INJECT_KEY: &str = "GOOSE_PLAN_EXEMPLARS_INJECT";
const K_KEY: &str = "GOOSE_PLAN_EXEMPLARS_K";
const CHAR_LIMIT_KEY: &str = "GOOSE_PLAN_EXEMPLARS_CHAR_LIMIT";
const DEFAULT_K: usize = 2;
const DEFAULT_CHAR_LIMIT: usize = 8_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ExemplarIndexRecord {
    pub(super) run_id: String,
    pub(super) task: String,
    pub(super) approved_at_ms: u128,
    pub(super) planner_provider: String,
    pub(super) planner_model: String,
    pub(super) planner_context_limit: Option<usize>,
    pub(super) path: String,
}

pub(super) struct ArchiveRequest<'a> {
    pub(super) run_id: &'a str,
    pub(super) task: &'a str,
    pub(super) plan_text: &'a str,
    pub(super) planner_provider: &'a str,
    pub(super) planner_model: &'a str,
    pub(super) planner_context_limit: Option<usize>,
    pub(super) approved_at_ms: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InjectionMode {
    Always,
    Never,
    Auto,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanExemplarInjection {
    pub(super) injected: bool,
    pub(super) selected_run_ids: Vec<String>,
    pub(super) prompt_section: Option<String>,
}

impl PlanExemplarInjection {
    pub(super) fn banner_fragment(&self) -> String {
        if self.injected {
            format!(
                " · exemplars injected [{}]",
                self.selected_run_ids.join(", ")
            )
        } else {
            " · exemplars skipped".to_string()
        }
    }
}

pub(super) fn build_injection(task: &str, planner_provider: &str) -> PlanExemplarInjection {
    if !exemplars_enabled() {
        return PlanExemplarInjection::default();
    }

    build_injection_from_state_dir(
        &Paths::state_dir(),
        task,
        planner_provider,
        injection_mode(),
        configured_k(),
        configured_char_limit(),
    )
}

pub(super) fn archive_approved_plan(approved: bool, request: &ArchiveRequest<'_>) -> bool {
    if !exemplars_enabled() {
        return false;
    }

    archive_approval_in_state_dir(&Paths::state_dir(), approved, request)
}

fn exemplars_enabled() -> bool {
    Config::global()
        .get_param::<bool>(ENABLED_KEY)
        .unwrap_or(true)
}

fn injection_mode() -> InjectionMode {
    match Config::global()
        .get_param::<String>(INJECT_KEY)
        .unwrap_or_else(|_| "auto".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "always" => InjectionMode::Always,
        "never" => InjectionMode::Never,
        _ => InjectionMode::Auto,
    }
}

fn configured_k() -> usize {
    Config::global()
        .get_param::<usize>(K_KEY)
        .ok()
        .filter(|k| *k > 0)
        .unwrap_or(DEFAULT_K)
}

fn configured_char_limit() -> usize {
    Config::global()
        .get_param::<usize>(CHAR_LIMIT_KEY)
        .ok()
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_CHAR_LIMIT)
}

fn should_inject(planner_provider: &str, mode: InjectionMode) -> bool {
    match mode {
        InjectionMode::Always => true,
        InjectionMode::Never => false,
        InjectionMode::Auto => !planner_provider.eq_ignore_ascii_case("claude-acp"),
    }
}

fn build_injection_from_state_dir(
    state_dir: &Path,
    task: &str,
    planner_provider: &str,
    mode: InjectionMode,
    k: usize,
    char_limit: usize,
) -> PlanExemplarInjection {
    if !should_inject(planner_provider, mode) {
        return PlanExemplarInjection::default();
    }

    let Some(records) = read_index_from_state_dir(state_dir) else {
        return PlanExemplarInjection::default();
    };
    let selected = select_similar_records(&records, task, k);
    if selected.is_empty() {
        return PlanExemplarInjection::default();
    }

    let mut selected_run_ids = Vec::new();
    let mut examples = String::from(
        "참고: 유사 과제에서 승인된 계획 예시 (형태를 참고하되 내용은 현재 과제 기준으로)\n\n",
    );

    for record in selected {
        let Ok(plan) = std::fs::read_to_string(&record.path) else {
            continue;
        };
        selected_run_ids.push(record.run_id.clone());
        examples.push_str(&format!(
            "예시 {} (run_id: {})\n<approved_plan_example run_id=\"{}\">\n{}\n</approved_plan_example>\n\n",
            selected_run_ids.len(),
            record.run_id,
            record.run_id,
            safe_truncate(&plan, char_limit)
        ));
    }

    if selected_run_ids.is_empty() {
        return PlanExemplarInjection::default();
    }

    PlanExemplarInjection {
        injected: true,
        selected_run_ids,
        prompt_section: Some(examples.trim_end().to_string()),
    }
}

fn select_similar_records(
    records: &[ExemplarIndexRecord],
    task: &str,
    k: usize,
) -> Vec<ExemplarIndexRecord> {
    if records.is_empty() || k == 0 {
        return Vec::new();
    }

    let query_tokens = tokenize(task);
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let mut scored = records
        .iter()
        .filter_map(|record| {
            let score = jaccard(&query_tokens, &tokenize(&record.task));
            (score > 0.0).then_some((score, record))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| right.approved_at_ms.cmp(&left.approved_at_ms))
    });

    scored
        .into_iter()
        .take(k)
        .map(|(_, record)| record.clone())
        .collect()
}

fn archive_approval_in_state_dir(
    state_dir: &Path,
    approved: bool,
    request: &ArchiveRequest<'_>,
) -> bool {
    if !approved {
        return false;
    }

    let dir = exemplars_dir(state_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }

    let plan_path = dir.join(format!("{}.md", request.run_id));
    if std::fs::write(&plan_path, request.plan_text).is_err() {
        return false;
    }

    let record = ExemplarIndexRecord {
        run_id: request.run_id.to_string(),
        task: request.task.to_string(),
        approved_at_ms: request.approved_at_ms,
        planner_provider: request.planner_provider.to_string(),
        planner_model: request.planner_model.to_string(),
        planner_context_limit: request.planner_context_limit,
        path: plan_path.display().to_string(),
    };
    let Ok(json) = serde_json::to_string(&record) else {
        return false;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(index_path(state_dir))
    else {
        return false;
    };

    writeln!(file, "{json}").is_ok()
}

fn read_index_from_state_dir(state_dir: &Path) -> Option<Vec<ExemplarIndexRecord>> {
    let content = match std::fs::read_to_string(index_path(state_dir)) {
        Ok(content) => content,
        Err(_) => return Some(Vec::new()),
    };

    let mut records = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<ExemplarIndexRecord>(line) else {
            return None;
        };
        records.push(record);
    }
    Some(records)
}

fn exemplars_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(EXEMPLARS_DIR)
}

fn index_path(state_dir: &Path) -> PathBuf {
    exemplars_dir(state_dir).join(INDEX_FILE)
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    intersection as f64 / union as f64
}

fn tokenize(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut ascii = String::new();
    let mut cjk = Vec::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut tokens);
            ascii.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut tokens);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut tokens);
            flush_cjk(&mut cjk, &mut tokens);
        }
    }

    flush_ascii(&mut ascii, &mut tokens);
    flush_cjk(&mut cjk, &mut tokens);
    tokens
}

fn flush_ascii(ascii: &mut String, tokens: &mut HashSet<String>) {
    if !ascii.is_empty() {
        tokens.insert(std::mem::take(ascii));
    }
}

fn flush_cjk(cjk: &mut Vec<char>, tokens: &mut HashSet<String>) {
    match cjk.len() {
        0 => {}
        1 => {
            tokens.insert(cjk[0].to_string());
        }
        _ => {
            for pair in cjk.windows(2) {
                tokens.insert(pair.iter().collect());
            }
        }
    }
    cjk.clear();
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn record(
        state_dir: &Path,
        run_id: &str,
        task: &str,
        approved_at_ms: u128,
    ) -> ExemplarIndexRecord {
        let path = state_dir
            .join("plan_exemplars")
            .join(format!("{run_id}.md"));
        ExemplarIndexRecord {
            run_id: run_id.to_string(),
            task: task.to_string(),
            approved_at_ms,
            planner_provider: "fable".to_string(),
            planner_model: "fable-5".to_string(),
            planner_context_limit: Some(200_000),
            path: path.display().to_string(),
        }
    }

    fn write_record_with_plan(
        state_dir: &Path,
        run_id: &str,
        task: &str,
        approved_at_ms: u128,
        plan: &str,
    ) {
        let record = record(state_dir, run_id, task, approved_at_ms);
        let path = std::path::PathBuf::from(&record.path);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, plan).expect("write plan");
        let index = state_dir.join("plan_exemplars").join("exemplars.jsonl");
        let mut line = serde_json::to_string(&record).expect("json");
        line.push('\n');
        fs::write(index, line).expect("write index");
    }

    #[test]
    fn select_similar_records_prioritizes_related_plan_tasks() {
        let state = tempfile::tempdir().expect("tempdir");
        let records = vec![
            record(
                state.path(),
                "window",
                "Fix desktop window resize jitter",
                300,
            ),
            record(
                state.path(),
                "orch-plan",
                "Add approved plan exemplar injection to the orch planner",
                100,
            ),
            record(
                state.path(),
                "orch-ledger",
                "Record selected plan exemplar run ids in the orch ledger",
                200,
            ),
        ];

        let selected = select_similar_records(
            &records,
            "Inject approved plan exemplars into the orch planner prompt",
            2,
        );

        assert_eq!(
            selected
                .iter()
                .map(|record| record.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["orch-plan", "orch-ledger"]
        );
    }

    #[test]
    fn select_similar_records_handles_cjk_bigram_matching() {
        let state = tempfile::tempdir().expect("tempdir");
        let records = vec![
            record(state.path(), "login", "로그인 화면 디자인 개선", 200),
            record(state.path(), "payment", "결제 오류 재시도 로직 수정", 100),
        ];

        let selected = select_similar_records(&records, "결제 실패 오류 처리 보강", 1);

        assert_eq!(selected[0].run_id, "payment");
    }

    #[test]
    fn select_similar_records_returns_empty_for_empty_store() {
        let selected = select_similar_records(&[], "anything", 2);

        assert!(selected.is_empty());
    }

    #[test]
    fn archive_approval_writes_only_approved_plans() {
        let state = tempfile::tempdir().expect("tempdir");
        let rejected = ArchiveRequest {
            run_id: "rejected",
            task: "Fix the rejected thing",
            plan_text: "do not save",
            planner_provider: "fable",
            planner_model: "fable-5",
            planner_context_limit: Some(200_000),
            approved_at_ms: 10,
        };

        assert!(!archive_approval_in_state_dir(
            state.path(),
            false,
            &rejected
        ));
        assert!(!state.path().join("plan_exemplars").exists());

        let approved = ArchiveRequest {
            run_id: "approved",
            task: "Fix the approved thing",
            plan_text: "approved plan",
            planner_provider: "fable",
            planner_model: "fable-5",
            planner_context_limit: Some(200_000),
            approved_at_ms: 20,
        };

        assert!(archive_approval_in_state_dir(state.path(), true, &approved));
        let plan_path = state.path().join("plan_exemplars").join("approved.md");
        assert_eq!(
            fs::read_to_string(plan_path).expect("plan"),
            "approved plan"
        );

        let records = read_index_from_state_dir(state.path()).expect("index");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "approved");
        assert_eq!(records[0].task, "Fix the approved thing");
        assert_eq!(records[0].approved_at_ms, 20);
        assert_eq!(records[0].planner_provider, "fable");
        assert_eq!(records[0].planner_model, "fable-5");
        assert_eq!(records[0].planner_context_limit, Some(200_000));
        assert!(records[0].path.ends_with("approved.md"));
    }

    #[test]
    fn injection_auto_skips_claude_acp_planner() {
        let state = tempfile::tempdir().expect("tempdir");
        write_record_with_plan(
            state.path(),
            "orch-plan",
            "Add approved plan exemplar injection to the orch planner",
            100,
            "approved plan shape",
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject approved plan exemplars into the orch planner prompt",
            "claude-acp",
            InjectionMode::Auto,
            2,
            8_000,
        );

        assert!(!injection.injected);
        assert!(injection.selected_run_ids.is_empty());
        assert!(injection.prompt_section.is_none());
    }
}
