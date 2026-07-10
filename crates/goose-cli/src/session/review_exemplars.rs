use goose::config::paths::Paths;
use goose::config::Config;
use goose::utils::{middle_out_truncate, safe_truncate};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::exemplars::{self, ExemplarInjection, InjectionMode, SimilarityRecord, StoreConfig};

const STORE: StoreConfig = StoreConfig {
    dir: "review_exemplars",
    key_prefix: "GOOSE_REVIEW_EXEMPLARS",
    default_k: 1,
    default_char_limit: 8_000,
};

const INJECTION_HEADER: &str =
    "참고: 유사 과제에서의 리뷰 예시 (판정 기준과 형식을 참고하되 판정은 현재 증거 기준으로)\n\n";
const REVIEW_LABELS: &[&str] = &["APPROVED", "REVISE"];

/// Knob controlling the implementer's "known failure modes" injection, distilled
/// from past REVISE reviews of similar tasks.
const FAILURE_MODES_KEY: &str = "GOOSE_IMPL_FAILURE_MODES";
const FAILURE_MODES_LABELS: &[&str] = &["REVISE"];
const FAILURE_MODES_K: usize = 2;
/// Total budget for the distilled failure-modes block, kept tail-preserving so
/// the earliest (most prominent) defects and the closing ones both survive.
const FAILURE_MODES_HEAD: usize = 1_000;
const FAILURE_MODES_TAIL: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ReviewExemplarIndexRecord {
    pub(super) run_id: String,
    pub(super) cycle: u32,
    pub(super) verdict: String,
    pub(super) task: String,
    pub(super) reviewed_at_ms: u128,
    pub(super) reviewer_provider: String,
    pub(super) reviewer_model: String,
    pub(super) reviewer_context_limit: Option<usize>,
    pub(super) path: String,
    #[serde(default)]
    pub(super) repo_root: Option<String>,
}

impl SimilarityRecord for ReviewExemplarIndexRecord {
    fn task(&self) -> &str {
        &self.task
    }

    fn recency_ms(&self) -> u128 {
        self.reviewed_at_ms
    }

    fn run_id(&self) -> &str {
        &self.run_id
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn label(&self) -> Option<&str> {
        Some(&self.verdict)
    }

    fn repo_root(&self) -> Option<&str> {
        self.repo_root.as_deref()
    }
}

pub(super) struct ArchiveReviewRequest<'a> {
    pub(super) run_id: &'a str,
    pub(super) cycle: u32,
    pub(super) verdict: &'a str,
    pub(super) task: &'a str,
    pub(super) review_text: &'a str,
    pub(super) reviewer_provider: &'a str,
    pub(super) reviewer_model: &'a str,
    pub(super) reviewer_context_limit: Option<usize>,
    pub(super) repo_root: Option<&'a str>,
    pub(super) reviewed_at_ms: u128,
}

pub(super) type ReviewExemplarInjection = ExemplarInjection;

#[derive(Clone, Copy)]
struct ReviewerServingModel<'a> {
    provider_name: &'a str,
    model: &'a str,
}

pub(super) fn build_injection(
    task: &str,
    reviewer_provider: &str,
    reviewer_model: &str,
    repo_root: Option<&str>,
    current_run_id: Option<&str>,
) -> ReviewExemplarInjection {
    if !STORE.enabled() {
        return ReviewExemplarInjection::default();
    }

    build_injection_from_state_dir(
        &Paths::state_dir(),
        task,
        ReviewerServingModel {
            provider_name: reviewer_provider,
            model: reviewer_model,
        },
        STORE.injection_mode(),
        STORE.k(),
        STORE.char_limit(),
        repo_root,
        current_run_id,
    )
}

/// Build the implementer-facing "known failure modes" injection: the distilled
/// defect lines from up to [`FAILURE_MODES_K`] REVISE reviews of tasks similar to
/// the current one. Gated exactly like the other uplift injections (frontier
/// serving models skip it in auto mode); honours `GOOSE_IMPL_FAILURE_MODES`.
pub(super) fn build_failure_modes_injection(
    task: &str,
    implementer_provider: &str,
    implementer_model: &str,
    repo_root: Option<&str>,
    current_run_id: Option<&str>,
) -> ReviewExemplarInjection {
    if !STORE.enabled() {
        return ReviewExemplarInjection::default();
    }

    build_failure_modes_from_state_dir(
        &Paths::state_dir(),
        task,
        ReviewerServingModel {
            provider_name: implementer_provider,
            model: implementer_model,
        },
        failure_modes_mode(),
        repo_root,
        current_run_id,
    )
}

pub(super) fn archive_review(request: &ArchiveReviewRequest<'_>) -> bool {
    archive_review_in_state_dir(&Paths::state_dir(), STORE.enabled(), request)
}

fn failure_modes_mode() -> InjectionMode {
    let raw = Config::global()
        .get_param::<String>(FAILURE_MODES_KEY)
        .unwrap_or_else(|_| "auto".to_string());
    exemplars::parse_injection_mode(&raw)
}

fn archive_review_in_state_dir(
    state_dir: &Path,
    enabled: bool,
    request: &ArchiveReviewRequest<'_>,
) -> bool {
    if !enabled {
        return false;
    }

    let file_name = format!("{}-review-c{}.md", request.run_id, request.cycle);
    let review_path = exemplars::artifact_path(state_dir, STORE.dir, &file_name);
    let record = ReviewExemplarIndexRecord {
        run_id: request.run_id.to_string(),
        cycle: request.cycle,
        verdict: request.verdict.to_string(),
        task: request.task.to_string(),
        reviewed_at_ms: request.reviewed_at_ms,
        reviewer_provider: request.reviewer_provider.to_string(),
        reviewer_model: request.reviewer_model.to_string(),
        reviewer_context_limit: request.reviewer_context_limit,
        path: review_path.display().to_string(),
        repo_root: request.repo_root.map(str::to_string),
    };

    exemplars::archive_text_and_record(
        state_dir,
        STORE.dir,
        &file_name,
        request.review_text,
        &record,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_injection_from_state_dir(
    state_dir: &Path,
    task: &str,
    reviewer: ReviewerServingModel<'_>,
    mode: InjectionMode,
    k: usize,
    char_limit: usize,
    repo_root: Option<&str>,
    current_run_id: Option<&str>,
) -> ReviewExemplarInjection {
    if !exemplars::should_inject(reviewer.provider_name, reviewer.model, mode) {
        return ReviewExemplarInjection::default();
    }

    let records = read_scoped_records(state_dir, current_run_id);
    let selected =
        exemplars::select_similar_records_by_label(&records, task, k, REVIEW_LABELS, repo_root);
    if selected.is_empty() {
        return ReviewExemplarInjection::default();
    }

    exemplars::assemble_injection(&selected, INJECTION_HEADER, |index, record, review| {
        format!(
            "예시 {} (run_id: {}, cycle: {}, verdict: {})\n<review_example run_id=\"{}\" cycle=\"{}\" verdict=\"{}\">\n{}\n</review_example>\n\n",
            index,
            record.run_id,
            record.cycle,
            record.verdict,
            record.run_id,
            record.cycle,
            record.verdict,
            safe_truncate(review, char_limit)
        )
    })
}

fn build_failure_modes_from_state_dir(
    state_dir: &Path,
    task: &str,
    implementer: ReviewerServingModel<'_>,
    mode: InjectionMode,
    repo_root: Option<&str>,
    current_run_id: Option<&str>,
) -> ReviewExemplarInjection {
    if !exemplars::should_inject(implementer.provider_name, implementer.model, mode) {
        return ReviewExemplarInjection::default();
    }

    let records = read_scoped_records(state_dir, current_run_id);
    let selected = exemplars::select_similar_records_by_label(
        &records,
        task,
        FAILURE_MODES_K,
        FAILURE_MODES_LABELS,
        repo_root,
    );
    if selected.is_empty() {
        return ReviewExemplarInjection::default();
    }

    let mut selected_run_ids = Vec::new();
    let mut defects = String::new();
    for record in selected {
        let Ok(review) = std::fs::read_to_string(&record.path) else {
            continue;
        };
        let block = extract_defect_lines(&review);
        if block.trim().is_empty() {
            continue;
        }
        selected_run_ids.push(record.run_id.clone());
        defects.push_str(&block);
        defects.push('\n');
    }
    if selected_run_ids.is_empty() {
        return ReviewExemplarInjection::default();
    }

    let capped = middle_out_truncate(defects.trim_end(), FAILURE_MODES_HEAD, FAILURE_MODES_TAIL);
    ReviewExemplarInjection {
        injected: true,
        selected_run_ids,
        prompt_section: Some(format!(
            "Known failure modes from past reviews — avoid these:\n{capped}"
        )),
    }
}

/// All index records for the store, minus any belonging to the run in flight so
/// a run never learns from its own not-yet-final reviews.
fn read_scoped_records(
    state_dir: &Path,
    current_run_id: Option<&str>,
) -> Vec<ReviewExemplarIndexRecord> {
    let mut records = exemplars::read_index::<ReviewExemplarIndexRecord>(state_dir, STORE.dir);
    if let Some(current_run_id) = current_run_id {
        records.retain(|record| record.run_id != current_run_id);
    }
    records
}

/// The defect body of an archived review: every non-empty line except the
/// `VERDICT:` marker line, so only the reviewer's concrete findings carry over.
fn extract_defect_lines(review: &str) -> String {
    review
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_verdict_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_verdict_line(line: &str) -> bool {
    line.to_ascii_lowercase().starts_with("verdict:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::exemplars::InjectionMode;
    use std::fs;
    use std::path::Path;

    fn reviewer<'a>(provider_name: &'a str, model: &'a str) -> ReviewerServingModel<'a> {
        ReviewerServingModel {
            provider_name,
            model,
        }
    }

    fn request<'a>(
        run_id: &'a str,
        cycle: u32,
        verdict: &'a str,
        review_text: &'a str,
    ) -> ArchiveReviewRequest<'a> {
        ArchiveReviewRequest {
            run_id,
            cycle,
            verdict,
            task: "Add review exemplar archive and injection",
            review_text,
            reviewer_provider: "fable",
            reviewer_model: "fable-5",
            reviewer_context_limit: Some(200_000),
            repo_root: Some("/repos/mine"),
            reviewed_at_ms: 123,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_review_record(
        state_dir: &Path,
        run_id: &str,
        cycle: u32,
        verdict: &str,
        task: &str,
        reviewed_at_ms: u128,
        review_text: &str,
    ) {
        write_review_record_in_repo(
            state_dir,
            run_id,
            cycle,
            verdict,
            task,
            reviewed_at_ms,
            review_text,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn write_review_record_in_repo(
        state_dir: &Path,
        run_id: &str,
        cycle: u32,
        verdict: &str,
        task: &str,
        reviewed_at_ms: u128,
        review_text: &str,
        repo_root: Option<&str>,
    ) {
        let path = state_dir
            .join("review_exemplars")
            .join(format!("{run_id}-review-c{cycle}.md"));
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, review_text).expect("write review");
        let record = ReviewExemplarIndexRecord {
            run_id: run_id.to_string(),
            cycle,
            verdict: verdict.to_string(),
            task: task.to_string(),
            reviewed_at_ms,
            reviewer_provider: "fable".to_string(),
            reviewer_model: "fable-5".to_string(),
            reviewer_context_limit: Some(200_000),
            path: path.display().to_string(),
            repo_root: repo_root.map(str::to_string),
        };
        let index = state_dir.join("review_exemplars").join("exemplars.jsonl");
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
    fn archive_review_writes_text_and_index_record() {
        let state = tempfile::tempdir().expect("tempdir");
        let req = request(
            "run-1",
            2,
            "REVISE",
            "VERDICT: REVISE\n\n1. crates/x.rs: missing gate rerun.",
        );

        assert!(archive_review_in_state_dir(state.path(), true, &req));

        let review_path = state
            .path()
            .join("review_exemplars")
            .join("run-1-review-c2.md");
        assert_eq!(
            fs::read_to_string(review_path).expect("review"),
            "VERDICT: REVISE\n\n1. crates/x.rs: missing gate rerun."
        );

        let records = exemplars::read_index::<ReviewExemplarIndexRecord>(state.path(), STORE.dir);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run-1");
        assert_eq!(records[0].cycle, 2);
        assert_eq!(records[0].verdict, "REVISE");
        assert_eq!(records[0].task, "Add review exemplar archive and injection");
        assert_eq!(records[0].reviewer_provider, "fable");
        assert_eq!(records[0].reviewer_model, "fable-5");
        assert_eq!(records[0].reviewer_context_limit, Some(200_000));
        assert_eq!(records[0].repo_root.as_deref(), Some("/repos/mine"));
        assert!(records[0].path.ends_with("run-1-review-c2.md"));
    }

    #[test]
    fn archive_review_respects_disabled_toggle() {
        let state = tempfile::tempdir().expect("tempdir");
        let req = request("run-1", 1, "APPROVED", "VERDICT: APPROVED");

        assert!(!archive_review_in_state_dir(state.path(), false, &req));
        assert!(!state.path().join("review_exemplars").exists());
    }

    #[test]
    fn archive_review_respects_goose_review_exemplars_false() {
        let root = tempfile::tempdir().expect("tempdir");
        let root_path = root.path().display().to_string();
        let _guard = env_lock::lock_env([
            ("GOOSE_REVIEW_EXEMPLARS", Some("false".to_string())),
            ("GOOSE_PATH_ROOT", Some(root_path)),
        ]);
        let req = request("run-1", 1, "APPROVED", "VERDICT: APPROVED");

        assert!(!archive_review(&req));
        assert!(!root.path().join("state").join("review_exemplars").exists());
    }

    #[test]
    fn injection_auto_injects_claude_acp_opus_reviewer() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Add review exemplar archive and injection",
            100,
            "VERDICT: APPROVED",
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "opus"),
            InjectionMode::Auto,
            1,
            8_000,
            None,
            None,
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["run-approved".to_string()]);
        assert!(injection
            .prompt_section
            .expect("prompt")
            .contains("VERDICT: APPROVED"));
    }

    #[test]
    fn injection_prefers_same_repo_reviews() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record_in_repo(
            state.path(),
            "cross-repo",
            1,
            "APPROVED",
            "Inject review exemplars into orch review prompt precisely",
            300,
            "VERDICT: APPROVED\ncross repo review",
            Some("/repos/other"),
        );
        write_review_record_in_repo(
            state.path(),
            "same-repo",
            1,
            "APPROVED",
            "Inject review exemplars into orch review prompt",
            100,
            "VERDICT: APPROVED\nsame repo review",
            Some("/repos/mine"),
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "opus"),
            InjectionMode::Auto,
            1,
            8_000,
            Some("/repos/mine"),
            None,
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["same-repo".to_string()]);
    }

    #[test]
    fn injection_auto_skips_fable_reviewer_models() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Add review exemplar archive and injection",
            100,
            "VERDICT: APPROVED",
        );

        for model in ["default", "claude-fable-5"] {
            let injection = build_injection_from_state_dir(
                state.path(),
                "Inject review exemplars into orch review prompt",
                reviewer("claude-acp", model),
                InjectionMode::Auto,
                1,
                8_000,
                None,
                None,
            );

            assert!(!injection.injected);
            assert!(injection.selected_run_ids.is_empty());
            assert!(injection.prompt_section.is_none());
        }
    }

    #[test]
    fn injection_explicit_modes_override_reviewer_model_identity() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Add review exemplar archive and injection",
            100,
            "VERDICT: APPROVED",
        );

        let always = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "claude-fable-5"),
            InjectionMode::Always,
            1,
            8_000,
            None,
            None,
        );
        assert!(always.injected);

        let never = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "opus"),
            InjectionMode::Never,
            1,
            8_000,
            None,
            None,
        );
        assert!(!never.injected);
    }

    #[test]
    fn injection_includes_approved_and_revise_examples_when_available() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Add review exemplar archive and injection",
            100,
            "VERDICT: APPROVED\nNo defects.",
        );
        write_review_record(
            state.path(),
            "run-revise",
            2,
            "REVISE",
            "Fix review exemplar archive missing revise verdict",
            200,
            "VERDICT: REVISE\n\n1. Missing archive call.",
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt and archive verdicts",
            reviewer("anthropic", "claude-opus"),
            InjectionMode::Auto,
            1,
            80,
            None,
            None,
        );

        assert!(injection.injected);
        assert_eq!(
            injection.selected_run_ids,
            vec!["run-approved".to_string(), "run-revise".to_string()]
        );
        let prompt = injection.prompt_section.expect("prompt");
        assert!(prompt.contains("유사 과제에서의 리뷰 예시"));
        assert!(prompt.contains("verdict=\"APPROVED\""));
        assert!(prompt.contains("verdict=\"REVISE\""));
        assert!(prompt.contains("cycle=\"2\""));
    }

    #[test]
    fn failure_modes_injects_defect_lines_from_similar_revise_reviews() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Inject review exemplars into orch review prompt",
            300,
            "VERDICT: APPROVED\nNo defects.",
        );
        write_review_record(
            state.path(),
            "run-revise",
            2,
            "REVISE",
            "Inject review exemplars into orch review prompt",
            200,
            "VERDICT: REVISE\n\n1. crates/x.rs: gate never re-run after fix.\n2. missing regression test.",
        );

        let injection = build_failure_modes_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "opus"),
            InjectionMode::Auto,
            None,
            None,
        );

        assert!(injection.injected);
        // Only the REVISE review contributes defect lines; APPROVED is excluded.
        assert_eq!(injection.selected_run_ids, vec!["run-revise".to_string()]);
        let prompt = injection.prompt_section.expect("prompt");
        assert!(prompt.contains("Known failure modes from past reviews"));
        assert!(prompt.contains("gate never re-run after fix"));
        assert!(!prompt.contains("VERDICT:"));
    }

    #[test]
    fn failure_modes_skips_frontier_implementer_in_auto() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-revise",
            1,
            "REVISE",
            "Inject review exemplars into orch review prompt",
            100,
            "VERDICT: REVISE\n\n1. defect here.",
        );

        let injection = build_failure_modes_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "claude-fable-5"),
            InjectionMode::Auto,
            None,
            None,
        );

        assert!(!injection.injected);
        assert!(injection.prompt_section.is_none());
    }

    #[test]
    fn failure_modes_empty_without_revise_history() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "run-approved",
            1,
            "APPROVED",
            "Inject review exemplars into orch review prompt",
            100,
            "VERDICT: APPROVED",
        );

        let injection = build_failure_modes_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("claude-acp", "opus"),
            InjectionMode::Auto,
            None,
            None,
        );

        assert!(!injection.injected);
    }

    #[test]
    fn injection_excludes_current_run_records() {
        let state = tempfile::tempdir().expect("tempdir");
        write_review_record(
            state.path(),
            "current-run",
            1,
            "REVISE",
            "Inject review exemplars into orch review prompt",
            300,
            "VERDICT: REVISE\n\n1. Current run defect.",
        );
        write_review_record(
            state.path(),
            "past-run",
            1,
            "REVISE",
            "Inject review exemplars into orch review prompt",
            100,
            "VERDICT: REVISE\n\n1. Past run defect.",
        );

        let injection = build_injection_from_state_dir(
            state.path(),
            "Inject review exemplars into orch review prompt",
            reviewer("anthropic", "claude-opus"),
            InjectionMode::Auto,
            1,
            8_000,
            None,
            Some("current-run"),
        );

        assert!(injection.injected);
        assert_eq!(injection.selected_run_ids, vec!["past-run".to_string()]);
        assert!(!injection
            .prompt_section
            .expect("prompt")
            .contains("Current run defect"));
    }
}
