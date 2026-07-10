use crate::session::output;
use crate::worktree::{self, CreatedWorktree, EnvLink, MergeResult};
use goose::config::Config;
use goose::utils::safe_truncate;
use std::path::{Path, PathBuf};

use super::phases::EVIDENCE_CHAR_LIMIT;

pub(crate) struct Evidence {
    pub(crate) text: String,
    pub(super) full: String,
    pub(super) truncated: bool,
}

pub(super) struct OrchWorkspace {
    pub(super) original_dir: PathBuf,
    pub(super) impl_dir: PathBuf,
    pub(super) repo_root: Option<PathBuf>,
    pub(super) branch: Option<String>,
    env_links: Vec<EnvLink>,
    pub(super) in_place_reason: Option<String>,
}

impl OrchWorkspace {
    fn in_place(original_dir: PathBuf, reason: impl Into<String>) -> Self {
        Self {
            impl_dir: original_dir.clone(),
            original_dir,
            repo_root: None,
            branch: None,
            env_links: Vec::new(),
            in_place_reason: Some(reason.into()),
        }
    }

    fn worktree(original_dir: PathBuf, repo_root: PathBuf, created: CreatedWorktree) -> Self {
        Self {
            original_dir,
            impl_dir: created.path,
            repo_root: Some(repo_root),
            branch: Some(created.branch),
            env_links: created.env_links,
            in_place_reason: None,
        }
    }

    pub(super) fn is_worktree(&self) -> bool {
        self.branch.is_some()
    }

    /// Whether ignored `.env*` files were symlinked into the worktree. Reflects
    /// the actual result, which the `GOOSE_ORCH_LINK_ENV` knob gates.
    pub(super) fn env_linked(&self) -> bool {
        !self.env_links.is_empty()
    }
}

pub(super) fn setup_orch_workspace(original_dir: &Path, run_id: &str) -> OrchWorkspace {
    let config = Config::global();
    let force_in_place = config
        .get_param::<bool>("GOOSE_ORCH_IN_PLACE")
        .unwrap_or(false);
    setup_orch_workspace_with_force(original_dir, run_id, force_in_place)
}

fn setup_orch_workspace_with_force(
    original_dir: &Path,
    run_id: &str,
    force_in_place: bool,
) -> OrchWorkspace {
    if force_in_place {
        return OrchWorkspace::in_place(original_dir.to_path_buf(), "GOOSE_ORCH_IN_PLACE=true");
    }

    let repo_root = match worktree::find_repo_root(original_dir) {
        Ok(repo_root) => repo_root,
        Err(_) => {
            return OrchWorkspace::in_place(original_dir.to_path_buf(), "not a git repository");
        }
    };

    let name = format!("orch-{run_id}");
    let branch = format!("orch/{run_id}");
    let link_env = Config::global()
        .get_param::<bool>("GOOSE_ORCH_LINK_ENV")
        .unwrap_or(true);
    match worktree::create_named_worktree_with_env_linking(
        original_dir,
        &name,
        Some(&branch),
        link_env,
    ) {
        Ok(created) => OrchWorkspace::worktree(original_dir.to_path_buf(), repo_root, created),
        Err(error) => OrchWorkspace::in_place(
            original_dir.to_path_buf(),
            format!("worktree creation failed: {error}"),
        ),
    }
}

fn display_workspace_path(workspace: &OrchWorkspace) -> String {
    workspace
        .repo_root
        .as_ref()
        .and_then(|repo_root| workspace.impl_dir.strip_prefix(repo_root).ok())
        .unwrap_or(&workspace.impl_dir)
        .display()
        .to_string()
}

pub(super) fn render_workspace_banner(workspace: &OrchWorkspace, auto_merge: bool) {
    if let Some(branch) = workspace.branch.as_deref() {
        println!(
            "{}",
            console::style(format!(
                "orchestrate workspace: worktree {} · branch {}{}",
                display_workspace_path(workspace),
                branch,
                if auto_merge {
                    " · auto-merge enabled"
                } else {
                    ""
                }
            ))
            .dim()
        );
        if !workspace.env_links.is_empty() {
            let linked = workspace
                .env_links
                .iter()
                .filter_map(|link| link.destination.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  {}",
                console::style(format!("linked env files: {linked}")).dim()
            );
        }
    } else {
        println!(
            "{}",
            console::style(format!(
                "orchestrate workspace: in-place: {} (no worktree - auto-commit/merge disabled)",
                workspace
                    .in_place_reason
                    .as_deref()
                    .unwrap_or("unknown reason")
            ))
            .yellow()
        );
    }
}

const UNTRACKED_FILE_CHAR_LIMIT: usize = 4_000;

pub(crate) fn git_evidence(dir: &Path) -> Evidence {
    let mut evidence = String::new();
    for args in [
        &["status", "--short"][..],
        &["diff", "HEAD"][..],
        &["diff", "--cached"][..],
    ] {
        if let Ok(out) = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                if !text.trim().is_empty() {
                    evidence.push_str(&format!("$ git {}\n{}\n", args.join(" "), text));
                }
            }
        }
    }
    evidence.push_str(&untracked_file_contents(dir));
    if evidence.trim().is_empty() {
        let text =
            "No git changes detected (not a git repository, or working tree clean).".to_string();
        return Evidence {
            full: text.clone(),
            text,
            truncated: false,
        };
    }
    let text = safe_truncate(&evidence, EVIDENCE_CHAR_LIMIT);
    let truncated = text.len() < evidence.len();
    Evidence {
        text,
        full: evidence,
        truncated,
    }
}

/// Contents of untracked (new) files, so the reviewer sees files that never
/// appear in `git diff HEAD`. Each file is capped, with a header noting the full
/// length when truncated. Mirrors the arena's evidence assembly.
fn untracked_file_contents(dir: &Path) -> String {
    let Ok(out) = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(dir)
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }

    let mut rendered = String::new();
    for file in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let Ok(content) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        let full_chars = content.chars().count();
        let capped = safe_truncate(&content, UNTRACKED_FILE_CHAR_LIMIT);
        let header = if full_chars > UNTRACKED_FILE_CHAR_LIMIT {
            format!("+++ new file: {file} (truncated, {full_chars} chars total)")
        } else {
            format!("+++ new file: {file}")
        };
        rendered.push_str(&format!("\n{header}\n{capped}\n"));
    }
    rendered
}

/// A concise `git diff --stat` for the review request, or empty when it yields
/// nothing (clean tree or non-git directory).
pub(crate) fn git_diff_stat(dir: &Path) -> String {
    let Ok(out) = std::process::Command::new("git")
        .args(["diff", "--stat", "HEAD"])
        .current_dir(dir)
        .output()
    else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn changed_paths_for_commit(dir: &Path) -> Vec<String> {
    worktree::git(
        dir,
        &[
            "-c",
            "core.quotePath=off",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    )
    .map(|status| {
        status
            .lines()
            .filter_map(parse_status_path)
            .filter(|path| path != ".goose-orch" && !path.starts_with(".goose-orch/"))
            .collect()
    })
    .unwrap_or_default()
}

fn parse_status_path(line: &str) -> Option<String> {
    let mut chars = line.chars();
    for _ in 0..3 {
        chars.next()?;
    }
    let path = chars.as_str();
    let path = path
        .split_once(" -> ")
        .map(|(_, renamed)| renamed)
        .unwrap_or(path);
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn conventional_commit_subject(task: &str, changed_paths: &[String]) -> String {
    const SUMMARY_LIMIT: usize = 65;

    let title_lower = commit_task_title(task).to_lowercase();
    let leading_token = title_lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .find(|token| !token.is_empty());
    let kind = if matches!(leading_token, Some("fix" | "bug" | "defect"))
        || title_lower.contains("버그")
        || title_lower.contains("결함")
        || title_lower.contains("고쳐")
        || title_lower.contains("고치")
        || title_lower.contains("수정")
    {
        "fix"
    } else if matches!(leading_token, Some("doc" | "docs")) || title_lower.contains("문서") {
        "docs"
    } else if matches!(leading_token, Some("test" | "tests")) {
        "test"
    } else if matches!(leading_token, Some("refactor" | "refactoring"))
        || title_lower.contains("리팩")
    {
        "refactor"
    } else {
        "feat"
    };

    let summary = commit_task_summary(task, kind, changed_paths);
    let summary = lowercase_first(summary);
    let summary = safe_truncate(&summary, SUMMARY_LIMIT);

    match common_scope(changed_paths) {
        Some(scope) => format!("{kind}({scope}): {summary}"),
        None => format!("{kind}: {summary}"),
    }
}

fn commit_task_summary(task: &str, kind: &str, changed_paths: &[String]) -> String {
    if let Some(summary) = english_summary_for_korean_task(task, kind, changed_paths) {
        return summary;
    }

    let title = commit_task_title(task);
    let summary = title.as_str();
    let summary = trim_explanatory_tail(summary)
        .trim_end_matches(['.', '!', '?'])
        .trim();
    let summary = trim_korean_request_suffix(summary).trim();
    if summary.is_empty() || contains_hangul(summary) {
        fallback_commit_summary(kind, changed_paths)
    } else {
        summary.to_string()
    }
}

fn commit_task_title(task: &str) -> String {
    task.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(strip_title_markers)
        .unwrap_or_else(|| "update orch lifecycle".to_string())
}

fn strip_title_markers(line: &str) -> String {
    let mut title = line.trim();
    title = title.trim_start_matches('#').trim_start();
    title = strip_task_label(title).trim_start();
    strip_surrounding_backticks(title)
}

fn strip_task_label(text: &str) -> &str {
    let text = text.trim_start();
    let task_len = "Task".len();
    if text
        .get(..task_len)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Task"))
    {
        if let Some(rest) = text.get(task_len..) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix(':') {
                return rest.trim_start();
            }
        }
    }
    if let Some(rest) = text.strip_prefix("과제") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            return rest.trim_start();
        }
    }
    text
}

fn strip_surrounding_backticks(text: &str) -> String {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('`') {
        if let Some(end) = rest.find('`') {
            let mut cleaned = String::with_capacity(text.len().saturating_sub(2));
            if let (Some(before), Some(after)) = (rest.get(..end), rest.get(end + 1..)) {
                cleaned.push_str(before);
                cleaned.push_str(after);
                return cleaned.trim().trim_matches('`').to_string();
            }
        }
    }
    text.trim_matches('`').to_string()
}

fn english_summary_for_korean_task(
    task: &str,
    kind: &str,
    changed_paths: &[String],
) -> Option<String> {
    if !contains_hangul(task) {
        return None;
    }
    let task_lower = task.to_lowercase();
    if task.contains("커밋") && (task.contains("메시지") || task.contains("제목")) {
        return Some("improve orch auto-commit titles".to_string());
    }
    if (task.contains("무활동") || task.contains("타임아웃")) && task.contains("계획") {
        return Some("retry short orch plans".to_string());
    }
    if (task_lower.contains("worktree") || task.contains("워크트리"))
        && task_lower.contains("prune")
    {
        return Some("prune merged orch worktrees".to_string());
    }
    if (task.contains("리팩") || task_lower.contains("refactor"))
        && (task_lower.contains("orchestrate.rs")
            || changed_paths.iter().any(|path| {
                path.ends_with("session/orchestrate.rs") || path.contains("session/orchestrate/")
            }))
    {
        return Some("split orch orchestration modules".to_string());
    }
    Some(fallback_commit_summary(kind, changed_paths))
}

fn trim_explanatory_tail(text: &str) -> &str {
    let markers = ["목적:", "Purpose:", "Acceptance criteria:", "완료 기준:"];
    markers
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
        .and_then(|index| text.get(..index))
        .unwrap_or(text)
}

fn trim_korean_request_suffix(text: &str) -> &str {
    text.trim_end()
        .trim_end_matches("해주세요")
        .trim_end_matches("해줘")
        .trim_end_matches("줘")
        .trim_end()
}

fn fallback_commit_summary(kind: &str, changed_paths: &[String]) -> String {
    if changed_paths.iter().any(|path| {
        path.ends_with("session/orchestrate.rs") || path.contains("session/orchestrate/")
    }) {
        match kind {
            "fix" => "fix orch orchestration".to_string(),
            "refactor" => "refactor orch orchestration".to_string(),
            _ => "update orch orchestration".to_string(),
        }
    } else {
        "update orch lifecycle".to_string()
    }
}

fn contains_hangul(text: &str) -> bool {
    text.chars().any(|ch| {
        ('\u{ac00}'..='\u{d7a3}').contains(&ch) || ('\u{3130}'..='\u{318f}').contains(&ch)
    })
}

fn lowercase_first(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return "update orch lifecycle".to_string();
    };
    first.to_lowercase().collect::<String>() + chars.as_str()
}

fn common_scope(paths: &[String]) -> Option<String> {
    let mut scopes = paths
        .iter()
        .filter_map(|path| path_scope(path))
        .filter(|scope| !scope.is_empty());
    let first = scopes.next()?;
    if scopes.all(|scope| scope == first) {
        Some(first)
    } else {
        None
    }
}

fn path_scope(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches("./");
    if path.starts_with("crates/goose-cli/") {
        Some("cli".to_string())
    } else if path.starts_with("crates/goose-mcp/") {
        Some("mcp".to_string())
    } else if path.starts_with("crates/goose/") {
        Some("goose".to_string())
    } else {
        path.split('/').next().map(ToString::to_string)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FinalizeOutcome {
    InPlace,
    NothingToMerge,
    ManualHint,
    Merged,
    Conflict,
    Error,
}

fn manual_merge_hint(branch: &str) -> String {
    format!("  to merge: git merge {branch}")
}

pub(super) fn finalize_worktree_approval(
    workspace: &OrchWorkspace,
    task: &str,
    auto_merge: bool,
) -> FinalizeOutcome {
    let Some(branch) = workspace.branch.as_deref() else {
        println!(
            "  {}",
            console::style("auto-commit skipped: in-place orchestration").dim()
        );
        return FinalizeOutcome::InPlace;
    };

    let changed_paths = changed_paths_for_commit(&workspace.impl_dir);
    let message = conventional_commit_subject(task, &changed_paths);
    let committed = match worktree::commit_all(&workspace.impl_dir, &message, &[".goose-orch"]) {
        Ok(true) => {
            println!(
                "{}",
                console::style(format!("orchestrate: committed {branch}: {message}"))
                    .green()
                    .bold()
            );
            true
        }
        Ok(false) => false,
        Err(error) => {
            output::render_error(&format!("Auto-commit failed: {error}"));
            return FinalizeOutcome::Error;
        }
    };

    let ahead = match worktree::commits_ahead(&workspace.original_dir, branch) {
        Ok(ahead) => ahead,
        Err(error) => {
            output::render_error(&format!("Failed to check commits awaiting merge: {error}"));
            return FinalizeOutcome::Error;
        }
    };

    if !committed && ahead == 0 {
        println!(
            "  {}",
            console::style("auto-commit skipped: no changes to commit").dim()
        );
        return FinalizeOutcome::NothingToMerge;
    }

    if !auto_merge {
        println!("{}", manual_merge_hint(branch));
        return FinalizeOutcome::ManualHint;
    }

    match worktree::merge_branch(&workspace.original_dir, branch) {
        Ok(MergeResult::Merged) => {
            println!(
                "{}",
                console::style(format!(
                    "orchestrate: merged {branch} into the original branch."
                ))
                .green()
                .bold()
            );
            if let Some(repo_root) = workspace.repo_root.as_deref() {
                if let Err(error) = worktree::remove_worktree(repo_root, &workspace.impl_dir, true)
                {
                    output::render_error(&format!(
                        "Merged, but failed to remove worktree {}: {}",
                        workspace.impl_dir.display(),
                        error
                    ));
                }
            }
            FinalizeOutcome::Merged
        }
        Ok(MergeResult::Conflict) => {
            output::render_error(&format!(
                "Auto-merge stopped because of conflicts. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
            FinalizeOutcome::Conflict
        }
        Err(error) => {
            output::render_error(&format!(
                "Auto-merge failed: {error}. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
            FinalizeOutcome::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        crate::worktree::git(dir, args).expect("git command");
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), &["init"]);
        git(temp.path(), &["config", "user.name", "Goose Test"]);
        git(temp.path(), &["config", "user.email", "goose@example.com"]);
        fs::write(temp.path().join(".gitignore"), ".env\n.goose/\n").expect("write gitignore");
        fs::write(temp.path().join("README.md"), "hello\n").expect("write readme");
        fs::write(temp.path().join(".env"), "ROOT=1\n").expect("write env");
        git(temp.path(), &["add", ".gitignore", "README.md"]);
        git(temp.path(), &["commit", "-m", "initial"]);
        temp
    }

    fn subject(task: &str, paths: &[&str]) -> String {
        super::conventional_commit_subject(
            task,
            &paths
                .iter()
                .map(|path| path.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn setup_orch_workspace_creates_named_worktree_branch_and_env_link() {
        // Serialize against tests that toggle GOOSE_ORCH_LINK_ENV; default (unset)
        // links env files.
        let _guard = env_lock::lock_env([("GOOSE_ORCH_LINK_ENV", None::<&str>)]);
        let repo = init_repo();
        let repo_root = crate::worktree::find_repo_root(repo.path()).expect("repo root");

        let workspace = super::setup_orch_workspace_with_force(repo.path(), "abc123", false);

        assert!(workspace.is_worktree());
        assert_eq!(
            workspace.impl_dir,
            repo_root.join(".goose/worktrees/orch-abc123")
        );
        assert_eq!(workspace.branch.as_deref(), Some("orch/abc123"));
        assert_eq!(workspace.env_links.len(), 1);
        assert_eq!(
            crate::worktree::current_branch(&workspace.impl_dir).expect("branch"),
            "orch/abc123"
        );
    }

    #[test]
    fn setup_orch_workspace_skips_env_link_when_knob_disabled() {
        let _guard = env_lock::lock_env([("GOOSE_ORCH_LINK_ENV", Some("false"))]);
        let repo = init_repo();

        let workspace = super::setup_orch_workspace(repo.path(), "no-env-link");

        assert!(workspace.is_worktree());
        assert!(
            !workspace.env_linked(),
            "GOOSE_ORCH_LINK_ENV=false must keep .env out of the worktree"
        );
        assert!(!workspace.impl_dir.join(".env").exists());
    }

    #[test]
    fn setup_orch_workspace_falls_back_in_place_when_forced_or_outside_git() {
        let repo = init_repo();
        let forced = super::setup_orch_workspace_with_force(repo.path(), "forced", true);
        assert!(!forced.is_worktree());
        assert_eq!(forced.impl_dir, repo.path());
        assert_eq!(
            forced.in_place_reason.as_deref(),
            Some("GOOSE_ORCH_IN_PLACE=true")
        );

        let temp = tempfile::tempdir().expect("tempdir");
        let non_git = super::setup_orch_workspace_with_force(temp.path(), "nongit", false);
        assert!(!non_git.is_worktree());
        assert_eq!(non_git.impl_dir, temp.path());
        assert_eq!(
            non_git.in_place_reason.as_deref(),
            Some("not a git repository")
        );
    }

    #[test]
    fn conventional_commit_subject_defaults_to_feat() {
        assert_eq!(
            subject("Add automatic orch worktrees", &["README.md"]),
            "feat(README.md): add automatic orch worktrees"
        );
    }

    #[test]
    fn conventional_commit_subject_infers_fix_type() {
        assert_eq!(
            subject(
                "Fix orch approval handling.",
                &["crates/goose-cli/src/session/orchestrate.rs"]
            ),
            "fix(cli): fix orch approval handling"
        );
    }

    #[test]
    fn conventional_commit_subject_omits_scope_for_mixed_changes() {
        assert_eq!(
            subject(
                "Refactor provider wiring",
                &[
                    "crates/goose-cli/src/session/orchestrate.rs",
                    "crates/goose/src/providers/base.rs"
                ]
            ),
            "refactor: refactor provider wiring"
        );
    }

    #[test]
    fn conventional_commit_subject_trims_blank_lines_and_long_summary() {
        assert_eq!(
            subject(
                "\n\nAdd a very long orchestration lifecycle summary that should be truncated before it turns into an unwieldy commit subject.",
                &["docs/reference.md"]
            ),
            "feat(docs): add a very long orchestration lifecycle summary that should be..."
        );
    }

    #[test]
    fn conventional_commit_subject_summarizes_korean_fix_task_in_english() {
        let task = "작은 결함 3건을 함께 고쳐줘. 목적: 승인 시 자동 커밋 메시지 생성이 과제 원문을 제목에 그대로 붙여 \"fix: ...해줘. 목적: ...\" 같은 저품질 제목이 나온다.";

        assert_eq!(
            subject(task, &["crates/goose-cli/src/session/orchestrate.rs"]),
            "fix(cli): improve orch auto-commit titles"
        );
    }

    #[test]
    fn conventional_commit_subject_cleans_markdown_task_heading() {
        let task = r#"# Task: `/loop` — time-based recurring prompt execution

Update loop scheduling behavior.

Do not classify this as a fix just because the body says fix failing CI.
Do not classify this as docs just because the body mentions docs/loops.md.
"#;

        assert_eq!(
            subject(task, &["crates/goose-cli/src/session/orchestrate.rs"]),
            "feat(cli): /loop — time-based recurring prompt execution"
        );
    }

    #[test]
    fn parse_status_path_handles_unicode_and_renames() {
        assert_eq!(
            super::parse_status_path("?? 문서.txt"),
            Some("문서.txt".to_string())
        );
        assert_eq!(
            super::parse_status_path("R  old.txt -> 새.txt"),
            Some("새.txt".to_string())
        );
    }

    #[test]
    fn finalize_merges_self_committed_changes_and_provides_manual_hint() {
        let repo = init_repo();
        let workspace = super::setup_orch_workspace_with_force(repo.path(), "self-commit", false);
        let original_head =
            crate::worktree::git(repo.path(), &["rev-parse", "HEAD"]).expect("original head");
        fs::write(workspace.impl_dir.join("self-committed.txt"), "approved\n")
            .expect("write implementation");
        git(&workspace.impl_dir, &["add", "self-committed.txt"]);
        git(
            &workspace.impl_dir,
            &["commit", "-m", "feat: implementer own commit"],
        );

        let manual_outcome =
            super::finalize_worktree_approval(&workspace, "Implement approved change", false);

        assert_eq!(manual_outcome, super::FinalizeOutcome::ManualHint);
        assert_eq!(
            super::manual_merge_hint("orch/self-commit"),
            "  to merge: git merge orch/self-commit"
        );
        assert_eq!(
            crate::worktree::git(repo.path(), &["rev-parse", "HEAD"]).expect("head after hint"),
            original_head
        );

        let merge_outcome =
            super::finalize_worktree_approval(&workspace, "Implement approved change", true);

        assert_eq!(merge_outcome, super::FinalizeOutcome::Merged);
        assert_eq!(
            fs::read_to_string(repo.path().join("self-committed.txt"))
                .expect("merged implementation"),
            "approved\n"
        );
        let log =
            crate::worktree::git(repo.path(), &["log", "--format=%s"]).expect("commit subjects");
        assert!(log
            .lines()
            .any(|line| line == "feat: implementer own commit"));
        assert_eq!(
            log.lines()
                .filter(|line| *line == "feat: implementer own commit")
                .count(),
            1
        );
    }

    #[test]
    fn finalize_merges_implementer_commit_and_uncommitted_changes() {
        let repo = init_repo();
        let workspace = super::setup_orch_workspace_with_force(repo.path(), "mixed", false);
        fs::write(workspace.impl_dir.join("committed.txt"), "implementer\n")
            .expect("write committed implementation");
        git(&workspace.impl_dir, &["add", "committed.txt"]);
        git(
            &workspace.impl_dir,
            &["commit", "-m", "feat: implementer own commit"],
        );
        fs::write(workspace.impl_dir.join("pending.txt"), "orchestrator\n")
            .expect("write pending implementation");

        let outcome =
            super::finalize_worktree_approval(&workspace, "Add mixed approval changes", true);

        assert_eq!(outcome, super::FinalizeOutcome::Merged);
        assert_eq!(
            fs::read_to_string(repo.path().join("committed.txt")).expect("committed file"),
            "implementer\n"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("pending.txt")).expect("pending file"),
            "orchestrator\n"
        );
        let log =
            crate::worktree::git(repo.path(), &["log", "--format=%s"]).expect("commit subjects");
        assert!(log
            .lines()
            .any(|line| line == "feat: implementer own commit"));
        assert!(log
            .lines()
            .any(|line| line == "feat(pending.txt): add mixed approval changes"));
    }

    #[test]
    fn git_evidence_includes_untracked_file_contents() {
        let repo = init_repo();
        fs::write(repo.path().join("brand_new.rs"), "fn added() {}\n").expect("write new file");

        let evidence = super::git_evidence(repo.path());

        assert!(evidence.text.contains("+++ new file: brand_new.rs"));
        assert!(evidence.text.contains("fn added() {}"));
    }

    #[test]
    fn git_diff_stat_reports_tracked_changes() {
        let repo = init_repo();
        fs::write(repo.path().join("README.md"), "hello\nworld\n").expect("modify tracked");

        let stat = super::git_diff_stat(repo.path());

        assert!(stat.contains("README.md"), "{stat}");
    }

    #[test]
    fn finalize_skips_merge_when_worktree_has_no_changes() {
        let repo = init_repo();
        let workspace = super::setup_orch_workspace_with_force(repo.path(), "no-change", false);
        let original_head =
            crate::worktree::git(repo.path(), &["rev-parse", "HEAD"]).expect("original head");

        let outcome =
            super::finalize_worktree_approval(&workspace, "No implementation changes", true);

        assert_eq!(outcome, super::FinalizeOutcome::NothingToMerge);
        assert_eq!(
            crate::worktree::git(repo.path(), &["rev-parse", "HEAD"]).expect("head after finalize"),
            original_head
        );
        assert!(workspace.impl_dir.exists());
    }
}
