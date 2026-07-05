mod agent;
mod auth;
mod cli;
mod config;
mod context;
mod mcp;
mod providers;
mod session;
mod skills;
mod tools;
mod ui_colors;
mod usage;

use anyhow::Result;
use clap::Parser;
use config::ToolSelection;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Args::parse();
    let mut config = config::Config::load()?;
    let mcp_enabled = if args.no_mcp {
        Some(false)
    } else if args.mcp.is_some() {
        Some(true)
    } else {
        None
    };
    let mcp_server_allow = args.mcp.clone().filter(|servers| !servers.is_empty());
    let tool_selection = if args.no_tools {
        Some(ToolSelection::None)
    } else {
        args.tools.clone().map(ToolSelection::List)
    };
    let tools_overridden = args.no_tools || args.tools.is_some();
    config.apply_cli_overrides(
        args.provider.as_deref(),
        args.model.as_deref(),
        args.thinking.as_deref(),
        args.safety.as_deref(),
        mcp_enabled,
        mcp_server_allow,
        tool_selection,
    )?;

    if let Some(command) = &args.command {
        match command {
            cli::Command::Login { provider }
                if provider == "openai" || provider == "openai-codex" =>
            {
                auth::openai_codex::login(&config).await?;
                return Ok(());
            }
            cli::Command::Login { provider } => {
                anyhow::bail!("unsupported login provider: {provider}")
            }
        }
    }

    if let Some(prompt) = args.print_prompt()? {
        agent::run_print(
            prompt,
            args.images.clone(),
            args.session.as_deref().or(args
                .resume
                .as_ref()
                .and_then(|reference| reference.as_deref())),
            args.title.as_deref(),
            &config,
        )
        .await?;
        return Ok(());
    }

    agent::run_interactive(
        &mut config,
        args.resume,
        args.r#continue,
        args.session,
        args.title.as_deref(),
        args.thinking.is_some(),
        args.safety.is_some(),
        tools_overridden,
    )
    .await
}
