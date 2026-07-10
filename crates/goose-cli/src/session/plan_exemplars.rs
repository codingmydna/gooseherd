use goose::config::paths::Paths;
use goose::utils::middle_out_truncate;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(super) use super::exemplars::InjectionMode;
use super::exemplars::{self, ExemplarInjection, SimilarityRecord, StoreConfig};

const STORE: StoreConfig = StoreConfig {
    dir: "plan_exemplars",
    key_prefix: "GOOSE_PLAN_EXEMPLARS",
    default_k: 2,
    default_char_limit: 8_000,
};

const INJECTION_HEADER: &str =
    "참고: 유사 과제에서 승인된 계획 예시 (형태를 참고하되 내용은 현재 과제 기준으로)\n\n";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ExemplarIndexRecord {
    pub(super) run_id: String,
    pub(super) task: String,
    pub(super) approved_at_ms: u128,
    pub(super) planner_provider: String,
    pub(super) planner_model: String,
    pub(super) planner_context_limit: Option<usize>,
    pub(super) path: String,
    #[serde(default)]
    pub(super) repo_root: Option<String>,
}

impl SimilarityRecord for ExemplarIndexRecord {
    fn task(&self) -> &str {
        &self.task
    }

    fn recency_ms(&self) -> u128 {
        self.approved_at_ms
    }

    fn run_id(&self) -> &str {
        &self.run_id
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn repo_root(&self) -> Option<&str> {
        self.repo_root.as_deref()
    }
}

pub(super) struct ArchiveRequest<'a> {
    pub(super) run_id: &'a str,
    pub(super) task: &'a str,
    pub(super) plan_text: &'a str,
    pub(super) planner_provider: &'a str,
    pub(super) planner_model: &'a str,
    pub(super) planner_context_limit: Option<usize>,
    pub(super) repo_root: Option<&'a str>,
    pub(super) approved_at_ms: u128,
}

pub(super) type PlanExemplarInjection = ExemplarInjection;

pub(super) fn build_injection(
    task: &str,
    planner_provider: &str,
    planner_model: &str,
    repo_root: Option<&str>,
) -> PlanExemplarInjection {
    if !STORE.enabled() {
        return PlanExemplarInjection::default();
    }

    build_injection_from_state_dir(
        &Paths::state_dir(),
        task,
        planner_provider,
        planner_model,
        STORE.injection_mode(),
        STORE.k(),
        STORE.char_limit(),
        repo_root,
    )
}

pub(super) fn archive_approved_plan(approved: bool, request: &ArchiveRequest<'_>) -> bool {
    if !STORE.enabled() {
        return false;
    }

    archive_approval_in_state_dir(&Paths::state_dir(), approved, request)
}

#[allow(clippy::too_many_arguments)]
fn build_injection_from_state_dir(
    state_dir: &Path,
    task: &str,
    planner_provider: &str,
    planner_model: &str,
    mode: InjectionMode,
    k: usize,
    char_limit: usize,
    repo_root: Option<&str>,
) -> PlanExemplarInjection {
    if !exemplars::should_inject(planner_provider, planner_model, mode) {
        return PlanExemplarInjection::default();
    }

    let records = exemplars::read_index::<ExemplarIndexRecord>(state_dir, STORE.dir);
    let selected = exemplars::select_similar_records(&records, task, k, repo_root);
    if selected.is_empty() {
        return PlanExemplarInjection::default();
    }

    // Acceptance criteria and verification commands live at the END of a plan,
    // so keep both ends: a smaller head for shape, a larger tail for the gates.
    let tail = char_limit * 5 / 8;
    let head = char_limit.saturating_sub(tail);
    exemplars::assemble_injection(&selected, INJECTION_HEADER, |index, record, plan| {
        format!(
            "예시 {} (run_id: {})\n<approved_plan_example run_id=\"{}\">\n{}\n</approved_plan_example>\n\n",
            index,
            record.run_id,
            record.run_id,
            middle_out_truncate(plan, head, tail)
        )
    })
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
    let plan_path = exemplars::artifact_path(state_dir, STORE.dir, &file_name);
    let record = ExemplarIndexRecord {
        run_id: request.run_id.to_string(),
        task: request.task.to_string(),
        approved_at_ms: request.approved_at_ms,
        planner_provider: request.planner_provider.to_string(),
        planner_model: request.planner_model.to_string(),
        planner_context_limit: request.planner_context_limit,
        path: plan_path.display().to_string(),
        repo_root: request.repo_root.map(str::to_string),
    };

    exemplars::archive_text_and_record(state_dir, STORE.dir, &file_name, request.plan_text, &record)
}

#[cfg(test)]
fn read_index_from_state_dir(state_dir: &Path) -> Vec<ExemplarIndexRecord> {
    exemplars::read_index(state_dir, STORE.dir)
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
            repo_root: None,
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
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(index)
            .expect("open index");
        file.write_all(line.as_bytes()).expect("write index");
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

        let selected = exemplars::select_similar_records(
            &records,
            "Inject approved plan exemplars into the orch planner prompt",
            2,
            None,
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

        let selected =
            exemplars::select_similar_records(&records, "결제 실패 오류 처리 보강", 1, None);

        assert_eq!(selected[0].run_id, "payment");
    }

    #[test]
    fn select_similar_records_returns_empty_for_empty_store() {
        let selected =
            exemplars::select_similar_records::<ExemplarIndexRecord>(&[], "anything", 2, None);

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
            repo_root: None,
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
            repo_root: Some("/repos/mine"),
            approved_at_ms: 20,
        };

        assert!(archive_approval_in_state_dir(state.path(), true, &approved));
        let plan_path = state.path().join("plan_exemplars").join("approved.md");
        assert_eq!(
            fs::read_to_string(plan_path).expect("plan"),
            "approved plan"
        );

        let records = read_index_from_state_dir(state.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "approved");
        assert_eq!(records[0].task, "Fix the approved thing");
        assert_eq!(records[0].approved_at_ms, 20);
        assert_eq!(records[0].planner_provider, "fable");
        assert_eq!(records[0].planner_model, "fable-5");
        assert_eq!(records[0].planner_context_limit, Some(200_000));
        assert_eq!(records[0].repo_root.as_deref(), Some("/repos/mine"));
        assert!(records[0].path.ends_with("approved.md"));
    }

    #[test]
    fn read_index_skips_corrupt_lines_without_aborting() {
        let state = tempfile::tempdir().expect("tempdir");
        write_record_with_plan(
            state.path(),
            "good",
            "a valid archived plan",
            100,
            "plan body",
        );
        let index = state.path().join("plan_exemplars").join("exemplars.jsonl");
        // A truncated / corrupt line must not disable the whole store.
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&index)
            .expect("open index");
        file.write_all(b"{not valid json\n")
            .expect("append corrupt");

        let records = read_index_from_state_dir(state.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "good");
    }

    #[test]
    fn injection_prefers_same_repo_exemplars() {
        let state = tempfile::tempdir().expect("tempdir");
        let mut cross = record(
            state.path(),
            "cross",
            "Inject approved plan exemplars into the orch planner prompt",
            300,
        );
        cross.repo_root = Some("/repos/other".to_string());
        let mut mine = record(
            state.path(),
            "mine",
            "Inject approved plan exemplars into the orch planner",
            100,
        );
        mine.repo_root = Some("/repos/mine".to_string());
        for rec in [&cross, &mine] {
            let path = std::path::PathBuf::from(&rec.path);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, format!("{} plan", rec.run_id)).expect("write plan");
            let index = state.path().join("plan_exemplars").join("exemplars.jsonl");
            let mut line = serde_json::to_string(rec).expect("json");
            line.push('\n');
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(index)
                .expect("open index");
            file.write_all(line.as_bytes()).expect("write index");
        }

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject approved plan exemplars into the orch planner prompt",
            "claude-acp",
            "opus",
            InjectionMode::Auto,
            1,
            8_000,
            Some("/repos/mine"),
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["mine".to_string()]);
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
            None,
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["orch-plan".to_string()]);
        assert!(injection
            .prompt_section
            .expect("prompt")
            .contains("approved plan shape"));
    }

    #[test]
    fn injection_tail_preserving_keeps_acceptance_criteria() {
        let state = tempfile::tempdir().expect("tempdir");
        // A long plan whose acceptance criteria live at the very end. The old
        // head-only truncation dropped them; middle-out must keep them.
        let mut plan = String::from("Files\n");
        plan.push_str(&"x".repeat(6_000));
        plan.push_str("\nAcceptance criteria: gate must rerun and pass\n");
        write_record_with_plan(
            state.path(),
            "orch-plan",
            "Add approved plan exemplar injection to the orch planner",
            100,
            &plan,
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject approved plan exemplars into the orch planner prompt",
            "claude-acp",
            "opus",
            InjectionMode::Auto,
            1,
            4_000,
            None,
        );

        let prompt = injection.prompt_section.expect("prompt");
        assert!(prompt.contains("truncated"));
        assert!(
            prompt.contains("Acceptance criteria: gate must rerun and pass"),
            "acceptance criteria at plan tail must survive truncation"
        );
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
                None,
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
            None,
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
            None,
        );
        assert!(!never.injected);
    }
}
