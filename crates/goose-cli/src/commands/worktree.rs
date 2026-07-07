use anyhow::Result;
use std::io::Write;

pub fn handle_new(name: String, branch: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let created = crate::worktree::create_named_worktree(&cwd, &name, branch.as_deref())?;

    println!("Created worktree:");
    println!("  Path: {}", created.path.display());
    println!("  Branch: {}", created.branch);
    if !created.env_links.is_empty() {
        let linked = created
            .env_links
            .iter()
            .filter_map(|link| link.destination.file_name())
            .map(|name| name.to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  Linked env files: {linked}");
    }
    println!("cd {} && goose", created.path.display());
    Ok(())
}

pub fn handle_list() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let entries = crate::worktree::list_goose_worktrees(&cwd)?;
    if entries.is_empty() {
        println!("No goose worktrees found under .goose/worktrees.");
        return Ok(());
    }

    let path_width = entries
        .iter()
        .map(|entry| entry.path.display().to_string().len())
        .max()
        .unwrap_or(4)
        .max("PATH".len());
    let branch_width = entries
        .iter()
        .map(|entry| branch_label(entry.branch.as_deref()).len())
        .max()
        .unwrap_or(6)
        .max("BRANCH".len());

    println!(
        "{:<path_width$}  {:<branch_width$}  DIRTY",
        "PATH", "BRANCH"
    );
    println!(
        "{:<path_width$}  {:<branch_width$}  -----",
        "-".repeat(path_width),
        "-".repeat(branch_width)
    );
    for entry in entries {
        println!(
            "{:<path_width$}  {:<branch_width$}  {}",
            entry.path.display(),
            branch_label(entry.branch.as_deref()),
            if entry.dirty { "yes" } else { "no" }
        );
    }
    Ok(())
}

pub fn handle_prune() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let candidates = crate::worktree::prunable_goose_worktrees(&cwd)?;
    if candidates.is_empty() {
        println!("No prunable goose worktrees found.");
        return Ok(());
    }

    println!("The following worktrees can be removed:");
    for candidate in &candidates {
        println!(
            "  {}  {}  {}",
            candidate.path.display(),
            branch_label(candidate.branch.as_deref()),
            candidate.reason
        );
    }
    print!("Remove these worktrees? [y/N] ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Aborted.");
        return Ok(());
    }

    crate::worktree::remove_worktrees(&cwd, &candidates)?;
    println!("Removed {} worktree(s).", candidates.len());
    Ok(())
}

fn branch_label(branch: Option<&str>) -> String {
    branch.unwrap_or("(detached)").to_string()
}
