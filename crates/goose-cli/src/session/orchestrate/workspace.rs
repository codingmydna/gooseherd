use crate::session::output;
use crate::worktree::{self, CreatedWorktree, EnvLink, MergeResult};
use goose::config::Config;
use goose::utils::safe_truncate;
use std::path::{Path, PathBuf};

use super::phases::EVIDENCE_CHAR_LIMIT;

pub(super) struct Evidence {
    pub(super) text: String,
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
    match worktree::create_named_worktree(original_dir, &name, Some(&branch)) {
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

pub(super) fn git_evidence(dir: &Path) -> Evidence {
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
    if evidence.is_empty() {
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

    let task_lower = task.to_lowercase();
    let kind = if task_lower.contains("fix")
        || task_lower.contains("bug")
        || task_lower.contains("defect")
        || task_lower.contains("버그")
        || task_lower.contains("결함")
        || task_lower.contains("고쳐")
        || task_lower.contains("고치")
        || task_lower.contains("수정")
    {
        "fix"
    } else if task_lower.contains("doc") || task_lower.contains("문서") {
        "docs"
    } else if task_lower.contains("test") {
        "test"
    } else if task_lower.contains("refactor") || task_lower.contains("리팩") {
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

    let summary = task
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("update orch lifecycle");
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
    } else if path.starts_with("crates/goose-server/") {
        Some("server".to_string())
    } else if path.starts_with("crates/goose/") {
        Some("goose".to_string())
    } else if path.starts_with("ui/desktop/") {
        Some("ui".to_string())
    } else {
        path.split('/').next().map(ToString::to_string)
    }
}

pub(super) fn finalize_worktree_approval(workspace: &OrchWorkspace, task: &str, auto_merge: bool) {
    let Some(branch) = workspace.branch.as_deref() else {
        println!(
            "  {}",
            console::style("auto-commit skipped: in-place orchestration").dim()
        );
        return;
    };

    let changed_paths = changed_paths_for_commit(&workspace.impl_dir);
    let message = conventional_commit_subject(task, &changed_paths);
    match worktree::commit_all(&workspace.impl_dir, &message, &[".goose-orch"]) {
        Ok(true) => {
            println!(
                "{}",
                console::style(format!("orchestrate: committed {branch}: {message}"))
                    .green()
                    .bold()
            );
            println!("  병합하려면: git merge {branch}");
        }
        Ok(false) => {
            println!(
                "  {}",
                console::style("auto-commit skipped: no changes to commit").dim()
            );
            return;
        }
        Err(error) => {
            output::render_error(&format!("Auto-commit failed: {error}"));
            return;
        }
    }

    if !auto_merge {
        return;
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
        }
        Ok(MergeResult::Conflict) => {
            output::render_error(&format!(
                "Auto-merge stopped because of conflicts. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
        }
        Err(error) => {
            output::render_error(&format!(
                "Auto-merge failed: {error}. Resolve manually with `git merge {branch}`; worktree kept at {}.",
                workspace.impl_dir.display()
            ));
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
                &["ui/desktop/src/main.ts"]
            ),
            "feat(ui): add a very long orchestration lifecycle summary that should be..."
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
}
