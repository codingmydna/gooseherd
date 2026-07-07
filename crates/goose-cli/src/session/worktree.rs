use anyhow::Result;

use super::{output, CliSession};

impl CliSession {
    pub(super) async fn handle_worktree(&mut self, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            output::render_error("Usage: /worktree <name>");
            return Ok(());
        }

        let cwd = std::env::current_dir()?;
        let created = crate::worktree::create_named_worktree(&cwd, name, None)?;
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
}
