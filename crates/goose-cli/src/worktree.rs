use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const WORKTREE_ROOT_REL: &str = ".goose/worktrees";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub env_links: Vec<EnvLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvLink {
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub dirty: bool,
    pub merged: bool,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunableWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeResult {
    Merged,
    Conflict,
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_os(dir, args.iter().map(OsStr::new))
}

pub fn git_os<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect();
    let output = Command::new("git")
        .args(&args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to run git {}", display_args(&args)))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            display_args(&args),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn find_repo_root(start: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .context("failed to run git rev-parse")?;

    if !output.status.success() {
        bail!("goose worktree requires a git repository. Run it from inside a git checkout.");
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        bail!("goose worktree requires a git repository. Run it from inside a git checkout.");
    }
    Ok(PathBuf::from(root))
}

pub fn current_branch(dir: &Path) -> Result<String> {
    Ok(git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string())
}

pub fn create_named_worktree(
    start: &Path,
    name: &str,
    branch: Option<&str>,
) -> Result<CreatedWorktree> {
    let repo_root = find_repo_root(start)?;
    let name = validate_name(name)?;
    let branch = match branch {
        Some(branch) if !branch.trim().is_empty() => branch.trim().to_string(),
        _ => format!("{}-{}", name, Local::now().format("%Y%m%d")),
    };
    validate_branch(&repo_root, &branch)?;

    let worktree_root = repo_root.join(WORKTREE_ROOT_REL);
    fs::create_dir_all(&worktree_root)?;
    ensure_worktree_root_gitignore(&worktree_root)?;
    let path = worktree_root.join(&name);
    if path.exists() {
        bail!("worktree `{name}` already exists at {}", path.display());
    }

    git_os(
        &repo_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(&branch),
            path.as_os_str(),
        ],
    )?;
    let env_links = symlink_ignored_env_files(&repo_root, &path)?;

    Ok(CreatedWorktree {
        path,
        branch,
        env_links,
    })
}

pub fn create_detached_worktree(repo_root: &Path, path: &Path, commit: &str) -> Result<()> {
    git_os(
        repo_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            path.as_os_str(),
            OsStr::new(commit),
        ],
    )?;
    Ok(())
}

pub fn remove_worktree(repo_root: &Path, path: &Path, force: bool) -> Result<()> {
    let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
    if force {
        args.push(OsString::from("--force"));
    }
    args.push(path.as_os_str().to_os_string());
    git_os(repo_root, args)?;
    Ok(())
}

pub fn commit_all(worktree: &Path, message: &str, exclude: &[&str]) -> Result<bool> {
    let mut add_args = vec![
        OsString::from("add"),
        OsString::from("-A"),
        OsString::from("--"),
        OsString::from("."),
    ];
    for path in exclude {
        let path = path.trim().trim_end_matches('/');
        if path.is_empty() {
            continue;
        }
        add_args.push(OsString::from(format!(":(exclude){path}")));
    }
    git_os(worktree, add_args)?;

    if git(worktree, &["diff", "--cached", "--name-only"])?
        .trim()
        .is_empty()
    {
        return Ok(false);
    }

    git_os(
        worktree,
        [OsStr::new("commit"), OsStr::new("-m"), OsStr::new(message)],
    )?;
    Ok(true)
}

pub fn merge_branch(dir: &Path, branch: &str) -> Result<MergeResult> {
    let output = Command::new("git")
        .args(["merge", "--no-ff", branch])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to run git merge --no-ff {branch}"))?;
    if output.status.success() {
        return Ok(MergeResult::Merged);
    }

    let _ = Command::new("git")
        .args(["merge", "--abort"])
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(MergeResult::Conflict)
}

pub fn list_goose_worktrees(start: &Path) -> Result<Vec<WorktreeInfo>> {
    let repo_root = find_repo_root(start)?;
    let output = git(&repo_root, &["worktree", "list", "--porcelain"])?;
    let worktree_root = repo_root.join(WORKTREE_ROOT_REL);
    let mut entries = Vec::new();

    for record in output.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in record.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(rest));
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
            }
        }

        let Some(path) = path else {
            continue;
        };
        if !path.starts_with(&worktree_root) {
            continue;
        }

        if !path.exists() {
            let merged = match branch.as_deref() {
                Some(branch) => branch_merged(&repo_root, branch)?,
                None => false,
            };
            entries.push(WorktreeInfo {
                path,
                branch,
                dirty: false,
                merged,
                missing: true,
            });
            continue;
        }

        let dirty = is_dirty(&path)?;
        let merged = match branch.as_deref() {
            Some(branch) => branch_merged(&repo_root, branch)?,
            None => false,
        };
        entries.push(WorktreeInfo {
            path,
            branch,
            dirty,
            merged,
            missing: false,
        });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub fn prunable_goose_worktrees(start: &Path) -> Result<Vec<PrunableWorktree>> {
    Ok(list_goose_worktrees(start)?
        .into_iter()
        .filter(|entry| !entry.dirty)
        .map(|entry| PrunableWorktree {
            reason: if entry.missing {
                "missing"
            } else if entry.merged {
                "merged"
            } else {
                "clean"
            }
            .to_string(),
            path: entry.path,
            branch: entry.branch,
        })
        .collect())
}

pub fn remove_worktrees(start: &Path, candidates: &[PrunableWorktree]) -> Result<()> {
    let repo_root = find_repo_root(start)?;
    for candidate in candidates {
        remove_worktree(&repo_root, &candidate.path, true)?;
    }
    let _ = git(&repo_root, &["worktree", "prune"]);
    Ok(())
}

pub fn symlink_ignored_env_files(repo_root: &Path, worktree_path: &Path) -> Result<Vec<EnvLink>> {
    let mut links = Vec::new();
    for entry in fs::read_dir(repo_root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !is_env_file_name(&file_name) || !is_git_ignored(repo_root, &file_name)? {
            continue;
        }

        let source = entry.path();
        let destination = worktree_path.join(file_name.as_ref());
        if fs::symlink_metadata(&destination).is_ok() {
            continue;
        }
        create_file_symlink(&source, &destination)?;
        links.push(EnvLink {
            source,
            destination,
        });
    }
    links.sort_by(|a, b| a.destination.cmp(&b.destination));
    Ok(links)
}

fn ensure_worktree_root_gitignore(worktree_root: &Path) -> Result<()> {
    let ignore_path = worktree_root.join(".gitignore");
    if !ignore_path.exists() {
        fs::write(ignore_path, "*\n")?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("worktree name is required");
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        bail!("worktree name `{name}` must not contain path separators");
    }
    if name.chars().any(char::is_control) {
        bail!("worktree name `{name}` must not contain control characters");
    }
    Ok(name.to_string())
}

fn validate_branch(repo_root: &Path, branch: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to validate git branch name")?;
    if !status.success() {
        bail!("invalid git branch name `{branch}`");
    }
    Ok(())
}

fn is_dirty(worktree_path: &Path) -> Result<bool> {
    Ok(!git(worktree_path, &["status", "--porcelain"])?
        .trim()
        .is_empty())
}

fn branch_merged(repo_root: &Path, branch: &str) -> Result<bool> {
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", branch, "HEAD"])
        .current_dir(repo_root)
        .status()
        .context("failed to check whether branch is merged")?;
    Ok(status.success())
}

fn is_git_ignored(repo_root: &Path, path: &str) -> Result<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "--quiet", "--", path])
        .current_dir(repo_root)
        .status()
        .context("failed to check ignored env file")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git check-ignore failed for `{path}`"),
    }
}

fn is_env_file_name(name: &str) -> bool {
    name == ".env" || name == ".env.local" || name.starts_with(".env.") && name.ends_with(".local")
}

#[cfg(unix)]
fn create_file_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, destination)?;
    Ok(())
}

#[cfg(windows)]
fn create_file_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::windows::fs::symlink_file(source, destination)?;
    Ok(())
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use chrono::Local;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git<I, S>(dir: &Path, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        init_repo_with_gitignore(".env\n.env.local\n.env.*.local\n.goose/\n")
    }

    fn init_repo_without_goose_ignore() -> tempfile::TempDir {
        init_repo_with_gitignore(".env\n.env.local\n.env.*.local\n")
    }

    fn init_repo_with_gitignore(gitignore: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), ["init"]);
        git(temp.path(), ["config", "user.name", "Goose Test"]);
        git(temp.path(), ["config", "user.email", "goose@example.com"]);
        fs::write(temp.path().join(".gitignore"), gitignore).expect("write gitignore");
        fs::write(temp.path().join("README.md"), "hello\n").expect("write readme");
        fs::write(temp.path().join(".env"), "ROOT=1\n").expect("write env");
        fs::write(temp.path().join(".env.local"), "LOCAL=1\n").expect("write env local");
        fs::write(temp.path().join(".env.dev.local"), "DEV=1\n").expect("write dev env");
        fs::write(temp.path().join(".env.notlocal"), "NOPE=1\n").expect("write ignored case");
        git(temp.path(), ["add", ".gitignore", "README.md"]);
        git(temp.path(), ["commit", "-m", "initial"]);
        temp
    }

    #[test]
    fn create_named_worktree_uses_default_branch_convention_and_symlinks_ignored_env_files() {
        let repo = init_repo();
        let today = Local::now().format("%Y%m%d");
        let repo_root = super::find_repo_root(repo.path()).expect("repo root");

        let created = super::create_named_worktree(repo.path(), "alpha", None).expect("created");

        assert_eq!(created.path, repo_root.join(".goose/worktrees/alpha"));
        assert_eq!(created.branch, format!("alpha-{today}"));
        assert_eq!(created.env_links.len(), 3);
        for filename in [".env", ".env.local", ".env.dev.local"] {
            let link = created.path.join(filename);
            assert!(
                fs::symlink_metadata(&link)
                    .expect("link metadata")
                    .file_type()
                    .is_symlink(),
                "{filename} should be a symlink"
            );
            assert_eq!(
                fs::read_to_string(&link).expect("read symlink"),
                fs::read_to_string(repo.path().join(filename)).expect("read source")
            );
        }
        assert!(!created.path.join(".env.notlocal").exists());

        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&created.path)
            .output()
            .expect("branch");
        assert_eq!(
            String::from_utf8_lossy(&branch.stdout).trim(),
            created.branch
        );
    }

    #[test]
    fn create_named_worktree_accepts_explicit_branch() {
        let repo = init_repo();
        let repo_root = super::find_repo_root(repo.path()).expect("repo root");

        let created =
            super::create_named_worktree(repo.path(), "beta", Some("topic/beta")).expect("created");

        assert_eq!(created.branch, "topic/beta");
        assert_eq!(created.path, repo_root.join(".goose/worktrees/beta"));
    }

    #[test]
    fn create_named_worktree_ignores_worktree_root_without_repo_gitignore_entry() {
        let repo = init_repo_without_goose_ignore();
        let repo_root = super::find_repo_root(repo.path()).expect("repo root");
        fs::remove_file(repo_root.join(".env.notlocal")).expect("remove unrelated untracked file");

        super::create_named_worktree(repo.path(), "gamma", Some("topic/gamma")).expect("created");

        let ignore_file = repo_root.join(".goose/worktrees/.gitignore");
        assert_eq!(
            fs::read_to_string(ignore_file).expect("read worktree gitignore"),
            "*\n"
        );
        let status = super::git(&repo_root, &["status", "--porcelain"]).expect("git status");
        assert!(status.trim().is_empty(), "unexpected git status: {status}");
    }

    #[test]
    fn list_goose_worktrees_reports_branch_and_dirty_state() {
        let repo = init_repo();
        let created = super::create_named_worktree(repo.path(), "dirty", Some("topic/dirty"))
            .expect("created");
        fs::write(created.path.join("README.md"), "changed\n").expect("dirty readme");

        let entries = super::list_goose_worktrees(repo.path()).expect("entries");
        let entry = entries
            .iter()
            .find(|entry| entry.path == created.path)
            .expect("created entry");

        assert_eq!(entry.branch.as_deref(), Some("topic/dirty"));
        assert!(entry.dirty);
    }

    #[test]
    fn prune_clean_goose_worktrees_removes_clean_entries_and_keeps_dirty_entries() {
        let repo = init_repo();
        let clean =
            super::create_named_worktree(repo.path(), "clean", Some("topic/clean")).expect("clean");
        let dirty =
            super::create_named_worktree(repo.path(), "dirty", Some("topic/dirty")).expect("dirty");
        fs::write(dirty.path.join("README.md"), "changed\n").expect("dirty readme");

        let candidates = super::prunable_goose_worktrees(repo.path()).expect("candidates");

        assert!(candidates
            .iter()
            .any(|candidate| candidate.path == clean.path));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.path == dirty.path));

        super::remove_worktrees(repo.path(), &candidates).expect("remove clean worktrees");
        assert!(!clean.path.exists());
        assert!(dirty.path.exists());
    }

    #[test]
    fn stale_goose_worktree_registration_is_listed_as_missing_and_prunable() {
        let repo = init_repo();
        let created =
            super::create_named_worktree(repo.path(), "stale", Some("topic/stale")).expect("stale");
        fs::remove_dir_all(&created.path).expect("remove worktree dir");

        let entries = super::list_goose_worktrees(repo.path()).expect("entries");
        let entry = entries
            .iter()
            .find(|entry| entry.path == created.path)
            .expect("stale entry");
        assert_eq!(entry.branch.as_deref(), Some("topic/stale"));
        assert!(!entry.dirty);
        assert!(entry.missing);

        let candidates = super::prunable_goose_worktrees(repo.path()).expect("candidates");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.path == created.path)
            .expect("stale candidate");
        assert_eq!(candidate.reason, "missing");

        super::remove_worktrees(repo.path(), &candidates).expect("remove stale worktree");
        assert!(super::list_goose_worktrees(repo.path())
            .expect("entries after remove")
            .is_empty());
    }

    #[test]
    fn find_repo_root_returns_friendly_error_outside_git_repo() {
        let temp = tempfile::tempdir().expect("tempdir");

        let error = super::find_repo_root(temp.path()).expect_err("should fail");

        assert!(error
            .to_string()
            .contains("goose worktree requires a git repository"));
    }

    #[test]
    fn current_branch_reports_checked_out_branch() {
        let repo = init_repo();

        git(repo.path(), ["checkout", "-b", "topic/current"]);

        assert_eq!(
            super::current_branch(repo.path()).expect("current branch"),
            "topic/current"
        );
    }

    #[test]
    fn commit_all_excludes_requested_paths_and_reports_empty_staging() {
        let repo = init_repo();
        let artifact_dir = repo.path().join(".goose-orch/run-1");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(repo.path().join("README.md"), "changed\n").expect("change readme");
        fs::write(artifact_dir.join("plan.md"), "plan\n").expect("write artifact");

        assert!(
            super::commit_all(repo.path(), "test: commit changes", &[".goose-orch"])
                .expect("commit")
        );

        let tree =
            super::git(repo.path(), &["ls-tree", "-r", "--name-only", "HEAD"]).expect("tree names");
        assert!(tree.contains("README.md"));
        assert!(!tree.contains(".goose-orch"));
        assert!(
            !super::commit_all(repo.path(), "test: empty", &[".goose-orch"]).expect("no commit")
        );
    }

    #[test]
    fn merge_branch_merges_committed_worktree_branch() {
        let repo = init_repo();
        let created = super::create_named_worktree(repo.path(), "merge", Some("topic/merge"))
            .expect("worktree");
        fs::write(created.path.join("README.md"), "changed in worktree\n").expect("change readme");
        assert!(
            super::commit_all(&created.path, "test: worktree change", &[]).expect("commit branch")
        );

        let result = super::merge_branch(repo.path(), "topic/merge").expect("merge");

        assert_eq!(result, super::MergeResult::Merged);
        assert_eq!(
            fs::read_to_string(repo.path().join("README.md")).expect("read readme"),
            "changed in worktree\n"
        );
    }

    #[test]
    fn merge_branch_reports_conflict_and_leaves_original_branch_clean() {
        let repo = init_repo();
        let original_branch = super::current_branch(repo.path()).expect("original branch");
        let created = super::create_named_worktree(repo.path(), "conflict", Some("topic/conflict"))
            .expect("worktree");
        fs::write(created.path.join("README.md"), "worktree\n").expect("worktree change");
        assert!(
            super::commit_all(&created.path, "test: worktree conflict", &[])
                .expect("commit branch")
        );
        fs::write(repo.path().join("README.md"), "original\n").expect("original change");
        assert!(
            super::commit_all(repo.path(), "test: original conflict", &[])
                .expect("commit original")
        );

        let result = super::merge_branch(repo.path(), "topic/conflict").expect("conflict result");

        assert_eq!(result, super::MergeResult::Conflict);
        assert_eq!(
            super::current_branch(repo.path()).expect("current branch"),
            original_branch
        );
        assert!(super::git(repo.path(), &["status", "--porcelain"])
            .expect("status")
            .trim()
            .is_empty());
    }
}
