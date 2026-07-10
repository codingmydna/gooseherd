//! Cached per-repository orientation block injected into the planner and
//! implementer instructions for non-frontier serving models. Cheap models most
//! often fail on repo orientation first; this gives them a capped skeleton, the
//! detected build gates, the conventions files, and the primary languages.

use anyhow::Result;
use goose::config::Config;
use goose::utils::{bytes_to_hex, safe_truncate};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::gates;
use super::roles::RoleConfig;
use crate::session::exemplars::{self, InjectionMode};

const REPO_PACK_KEY: &str = "GOOSE_REPO_PACK";
const REPO_PACK_CHAR_LIMIT: usize = 4_000;
const MAX_ENTRIES_PER_DIR: usize = 20;
const SKELETON_DEPTH: usize = 2;
const CONVENTIONS_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "README.md"];
const CONVENTIONS_HEAD_LINES: usize = 10;

fn repo_pack_mode() -> InjectionMode {
    let raw = Config::global()
        .get_param::<String>(REPO_PACK_KEY)
        .unwrap_or_else(|_| "auto".to_string());
    exemplars::parse_injection_mode(&raw)
}

pub(super) fn repo_pack_injects(role: &RoleConfig) -> bool {
    repo_pack_injects_with_mode(role, repo_pack_mode())
}

fn repo_pack_injects_with_mode(role: &RoleConfig, mode: InjectionMode) -> bool {
    exemplars::should_inject(&role.provider_name, &role.model, mode)
}

/// The clearly-delimited orientation block appended to a role's instructions.
pub(super) fn orientation_block(pack: &str) -> String {
    format!("\n\n## Repository orientation\n\n{pack}\n\n(End of repository orientation.)")
}

/// Return the repo pack for `repo_root`, served from the per-root cache when the
/// git HEAD stamp still matches, otherwise rebuilt and re-cached. Falls back to a
/// fresh build (uncached) on any cache IO error, and to `None` if the build
/// itself fails.
pub(super) fn cached_repo_pack(repo_root: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let head = git_head(&canonical).unwrap_or_else(|| "no-head".to_string());
    let cache_path = cache_path_for(&canonical);

    if let Some(cached) = read_cache(&cache_path, &head) {
        return Some(cached);
    }
    let pack = build_repo_pack(&canonical).ok()?;
    write_cache(&cache_path, &head, &pack);
    Some(pack)
}

pub(super) fn build_repo_pack(repo_root: &Path) -> Result<String> {
    let mut out = String::new();
    out.push_str("Repository: ");
    out.push_str(&repo_display_name(repo_root));
    out.push('\n');

    out.push_str("\n### Directory skeleton (top 2 levels)\n");
    out.push_str(&directory_skeleton(repo_root));

    out.push_str("\n### Build & gates\n");
    out.push_str(&manifests_and_gates(repo_root));

    out.push_str("\n### Primary languages\n");
    out.push_str(&primary_languages(repo_root));

    if let Some(conventions) = conventions_headline(repo_root) {
        out.push_str("\n### Conventions\n");
        out.push_str(&conventions);
        out.push('\n');
    }

    Ok(safe_truncate(out.trim_end(), REPO_PACK_CHAR_LIMIT))
}

fn repo_display_name(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| repo_root.display().to_string())
}

struct SkeletonEntry {
    name: String,
    is_dir: bool,
}

fn directory_skeleton(repo_root: &Path) -> String {
    let children = collect_children(repo_root, SKELETON_DEPTH);
    let mut out = String::new();
    render_children(&children, Path::new(""), "", true, &mut out);
    if out.is_empty() {
        out.push_str("(empty or fully ignored)\n");
    }
    out
}

fn collect_children(repo_root: &Path, max_depth: usize) -> BTreeMap<PathBuf, Vec<SkeletonEntry>> {
    let mut map: BTreeMap<PathBuf, Vec<SkeletonEntry>> = BTreeMap::new();
    for result in WalkBuilder::new(repo_root)
        .max_depth(Some(max_depth))
        .build()
    {
        let Ok(entry) = result else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(repo_root) else {
            continue;
        };
        let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        map.entry(parent)
            .or_default()
            .push(SkeletonEntry { name, is_dir });
    }
    map
}

fn render_children(
    children: &BTreeMap<PathBuf, Vec<SkeletonEntry>>,
    key: &Path,
    indent: &str,
    expand: bool,
    out: &mut String,
) {
    let Some(entries) = children.get(key) else {
        return;
    };
    let mut sorted: Vec<&SkeletonEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    let extra = sorted.len().saturating_sub(MAX_ENTRIES_PER_DIR);
    for entry in sorted.iter().take(MAX_ENTRIES_PER_DIR) {
        out.push_str(indent);
        out.push_str(&entry.name);
        if entry.is_dir {
            out.push('/');
        }
        out.push('\n');
        if expand && entry.is_dir {
            let child_key = key.join(&entry.name);
            let child_indent = format!("{indent}  ");
            render_children(children, &child_key, &child_indent, false, out);
        }
    }
    if extra > 0 {
        out.push_str(indent);
        out.push_str(&format!("… (+{extra} more)\n"));
    }
}

fn manifests_and_gates(repo_root: &Path) -> String {
    let mut out = String::new();
    let manifests = detect_manifests(repo_root);
    if manifests.is_empty() {
        out.push_str("manifests: none detected\n");
    } else {
        out.push_str(&format!("manifests: {}\n", manifests.join(", ")));
    }

    let resolved = gates::resolve_gates(
        repo_root,
        None,
        Config::global()
            .get_param::<Vec<String>>("GOOSE_ORCH_GATES")
            .unwrap_or_default(),
    );
    let partition = gates::partition_gates(repo_root, &resolved.gates);
    if partition.applicable.is_empty() {
        out.push_str("gates: none applicable\n");
    } else {
        out.push_str(&format!("gates: {}\n", partition.applicable.join("; ")));
    }
    out
}

fn detect_manifests(repo_root: &Path) -> Vec<String> {
    const CANDIDATES: [&str; 7] = [
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
    ];
    CANDIDATES
        .into_iter()
        .filter(|name| repo_root.join(name).is_file())
        .map(ToString::to_string)
        .collect()
}

fn primary_languages(repo_root: &Path) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in tracked_files(repo_root) {
        if let Some(ext) = Path::new(&file)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
        {
            *counts.entry(ext).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return "(none detected)\n".to_string();
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let listed = ranked
        .into_iter()
        .take(5)
        .map(|(ext, count)| format!(".{ext} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{listed}\n")
}

fn tracked_files(repo_root: &Path) -> Vec<String> {
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(repo_root)
        .output()
    {
        if output.status.success() {
            let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(ToString::to_string)
                .collect();
            if !files.is_empty() {
                return files;
            }
        }
    }
    WalkBuilder::new(repo_root)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(repo_root)
                .ok()
                .map(|rel| rel.display().to_string())
        })
        .collect()
}

fn conventions_headline(repo_root: &Path) -> Option<String> {
    let mut out = String::new();
    for name in CONVENTIONS_FILES {
        let Ok(content) = std::fs::read_to_string(repo_root.join(name)) else {
            continue;
        };
        let head: Vec<&str> = content.lines().take(CONVENTIONS_HEAD_LINES).collect();
        if head.iter().any(|line| !line.trim().is_empty()) {
            out.push_str(&format!(
                "{name} (first {} lines):\n{}\n\n",
                head.len(),
                head.join("\n")
            ));
        }
    }
    (!out.is_empty()).then(|| out.trim_end().to_string())
}

fn git_head(repo_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!head.is_empty()).then_some(head)
}

fn cache_path_for(canonical_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(canonical_root.display().to_string().as_bytes());
    let hash = bytes_to_hex(hasher.finalize());
    goose::config::paths::Paths::state_dir()
        .join("repo_packs")
        .join(format!("{hash}.md"))
}

fn read_cache(path: &Path, head: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let (stamp, body) = content.split_once('\n')?;
    (stamp == head).then(|| body.to_string())
}

fn write_cache(path: &Path, head: &str, pack: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{head}\n{pack}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn role(provider_name: &str, model: &str) -> RoleConfig {
        RoleConfig {
            provider_name: provider_name.to_string(),
            model: model.to_string(),
            effort: None,
        }
    }

    #[test]
    fn skeleton_respects_gitignore_and_caps_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        fs::write(root.join(".gitignore"), "ignored_dir/\nsecret.txt\n").expect("gitignore");
        fs::create_dir(root.join("src")).expect("src");
        fs::write(root.join("src/lib.rs"), "// lib").expect("lib");
        fs::create_dir(root.join("ignored_dir")).expect("ignored dir");
        fs::write(root.join("ignored_dir/nope.rs"), "// nope").expect("nope");
        fs::write(root.join("secret.txt"), "shh").expect("secret");
        // 25 sibling files to exceed the per-directory cap.
        for index in 0..25 {
            fs::write(root.join(format!("file{index:02}.md")), "x").expect("file");
        }
        // gitignore only applies inside a git repo.
        assert!(std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init")
            .status
            .success());

        let skeleton = super::directory_skeleton(root);

        assert!(skeleton.contains("src/"));
        assert!(skeleton.contains("lib.rs"));
        assert!(!skeleton.contains("ignored_dir"));
        assert!(!skeleton.contains("secret.txt"));
        assert!(skeleton.contains("… (+"));
    }

    #[test]
    fn build_repo_pack_reports_manifests_and_languages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("manifest");
        fs::create_dir(root.join("src")).expect("src");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("main");
        fs::write(root.join("src/lib.rs"), "// lib").expect("lib");

        let pack = super::build_repo_pack(root).expect("pack");

        assert!(pack.contains("### Directory skeleton"));
        assert!(pack.contains("manifests: Cargo.toml"));
        assert!(pack.contains(".rs ("));
        assert!(pack.chars().count() <= REPO_PACK_CHAR_LIMIT);
    }

    #[test]
    fn cache_stamp_invalidates_on_head_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_path = temp.path().join("pack.md");
        super::write_cache(&cache_path, "head-a", "PACK A");

        assert_eq!(
            super::read_cache(&cache_path, "head-a").as_deref(),
            Some("PACK A")
        );
        assert_eq!(super::read_cache(&cache_path, "head-b"), None);
        assert_eq!(
            super::read_cache(temp.path().join("missing.md").as_path(), "head-a"),
            None
        );
    }

    #[test]
    fn injection_gating_follows_mode_and_frontier() {
        let _guard =
            env_lock::lock_env([("GOOSE_UPLIFT_FRONTIER_PATTERNS", Some("fable".to_string()))]);
        // Auto: a non-frontier model gets the pack, a frontier one does not.
        assert!(super::repo_pack_injects_with_mode(
            &role("openai", "gpt-5.5"),
            InjectionMode::Auto
        ));
        assert!(!super::repo_pack_injects_with_mode(
            &role("anthropic", "claude-fable-5"),
            InjectionMode::Auto
        ));
        // Explicit modes override the frontier presumption.
        assert!(super::repo_pack_injects_with_mode(
            &role("anthropic", "claude-fable-5"),
            InjectionMode::Always
        ));
        assert!(!super::repo_pack_injects_with_mode(
            &role("openai", "gpt-5.5"),
            InjectionMode::Never
        ));
    }

    #[test]
    fn orientation_block_is_delimited() {
        let block = super::orientation_block("SKELETON");
        assert!(block.contains("## Repository orientation"));
        assert!(block.contains("SKELETON"));
        assert!(block.contains("End of repository orientation"));
    }
}
