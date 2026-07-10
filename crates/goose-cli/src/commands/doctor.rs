use anyhow::Result;
use goose::config::Config;

use crate::session::build_session;
use crate::session::SessionBuilderConfig;

pub async fn handle_doctor() -> Result<()> {
    let config = Config::global();
    crate::commands::herd::print_environment_diagnostics(config);
    println!();

    let can_build = config.get_goose_provider().is_ok() && config.get_goose_model().is_ok();
    if !can_build {
        println!("session diagnostics skipped (no provider configured)");
        println!("Run `goose herd` (recommended) or `goose configure` to set up a provider.");
        return Ok(());
    }

    let mut session = build_session(SessionBuilderConfig {
        no_session: true,
        interactive: true,
        ..Default::default()
    })
    .await;

    session.interactive(Some("/doctor".to_string())).await
}
