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

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Args::parse();
    let mut config = config::Config::load()?;
    let mcp_enabled = if args.mcp {
        Some(true)
    } else if args.no_mcp {
        Some(false)
    } else {
        None
    };
    config.apply_cli_overrides(
        args.provider.as_deref(),
        args.model.as_deref(),
        args.thinking.as_deref(),
        mcp_enabled,
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

    if let Some(prompt) = args.print_prompt() {
        agent::run_print(prompt, args.images.clone(), &config).await?;
        return Ok(());
    }

    agent::run_interactive(
        &mut config,
        args.resume,
        args.r#continue,
        args.session,
        args.thinking.is_some(),
    )
    .await
}
