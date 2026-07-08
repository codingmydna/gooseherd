use goose::config::{paths::Paths, Config};
use goose::utils::safe_truncate;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(super) use super::exemplars::InjectionMode;
use super::exemplars::{self, ExemplarInjection, SimilarityRecord};

const EXEMPLARS_DIR: &str = "plan_exemplars";
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

impl SimilarityRecord for ExemplarIndexRecord {
    fn task(&self) -> &str {
        &self.task
    }

    fn recency_ms(&self) -> u128 {
        self.approved_at_ms
    }
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

pub(super) type PlanExemplarInjection = ExemplarInjection;

pub(super) fn build_injection(
    task: &str,
    planner_provider: &str,
    planner_model: &str,
) -> PlanExemplarInjection {
    if !exemplars_enabled() {
        return PlanExemplarInjection::default();
    }

    build_injection_from_state_dir(
        &Paths::state_dir(),
        task,
        planner_provider,
        planner_model,
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
    let raw = Config::global()
        .get_param::<String>(INJECT_KEY)
        .unwrap_or_else(|_| "auto".to_string());
    exemplars::parse_injection_mode(&raw)
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

fn build_injection_from_state_dir(
    state_dir: &Path,
    task: &str,
    planner_provider: &str,
    planner_model: &str,
    mode: InjectionMode,
    k: usize,
    char_limit: usize,
) -> PlanExemplarInjection {
    if !exemplars::should_inject(planner_provider, planner_model, mode) {
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
    exemplars::select_similar_records(records, task, k)
}

fn archive_approval_in_state_dir(
    state_dir: &Path,
    approved: bool,
    request: &ArchiveRequest<'_>,
) -> bool {
    if !approved {
        return false;
    }

    let file_name = format!("{}.md", request.run_id);
    let plan_path = exemplars::artifact_path(state_dir, EXEMPLARS_DIR, &file_name);
    let record = ExemplarIndexRecord {
        run_id: request.run_id.to_string(),
        task: request.task.to_string(),
        approved_at_ms: request.approved_at_ms,
        planner_provider: request.planner_provider.to_string(),
        planner_model: request.planner_model.to_string(),
        planner_context_limit: request.planner_context_limit,
        path: plan_path.display().to_string(),
    };

    exemplars::archive_text_and_record(
        state_dir,
        EXEMPLARS_DIR,
        &file_name,
        request.plan_text,
        &record,
    )
}

fn read_index_from_state_dir(state_dir: &Path) -> Option<Vec<ExemplarIndexRecord>> {
    exemplars::read_index(state_dir, EXEMPLARS_DIR)
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
    fn injection_auto_injects_claude_acp_opus_planner() {
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
            "opus",
            InjectionMode::Auto,
            2,
            8_000,
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["orch-plan".to_string()]);
        assert!(injection
            .prompt_section
            .expect("prompt")
            .contains("approved plan shape"));
    }

    #[test]
    fn injection_auto_skips_fable_planner_models() {
        let state = tempfile::tempdir().expect("tempdir");
        write_record_with_plan(
            state.path(),
            "orch-plan",
            "Add approved plan exemplar injection to the orch planner",
            100,
            "approved plan shape",
        );

        for model in ["default", "claude-fable-5"] {
            let injection = build_injection_from_state_dir(
                state.path(),
                "Inject approved plan exemplars into the orch planner prompt",
                "claude-acp",
                model,
                InjectionMode::Auto,
                2,
                8_000,
            );

            assert!(!injection.injected);
            assert!(injection.selected_run_ids.is_empty());
            assert!(injection.prompt_section.is_none());
        }
    }

    #[test]
    fn injection_explicit_modes_override_planner_model_identity() {
        let state = tempfile::tempdir().expect("tempdir");
        write_record_with_plan(
            state.path(),
            "orch-plan",
            "Add approved plan exemplar injection to the orch planner",
            100,
            "approved plan shape",
        );

        let always = build_injection_from_state_dir(
            state.path(),
            "Inject approved plan exemplars into the orch planner prompt",
            "claude-acp",
            "claude-fable-5",
            InjectionMode::Always,
            2,
            8_000,
        );
        assert!(always.injected);

        let never = build_injection_from_state_dir(
            state.path(),
            "Inject approved plan exemplars into the orch planner prompt",
            "claude-acp",
            "opus",
            InjectionMode::Never,
            2,
            8_000,
        );
        assert!(!never.injected);
    }
}
