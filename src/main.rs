mod acp;
pub mod agent;
mod atomic_file;
mod auth;
mod cancel;
mod cli;
mod config;
mod context;
mod mcp;
mod persistence;
mod picker;
mod process_containment;
mod providers;
mod session;
mod skills;
mod terminal_text;
mod text_truncate;
mod tools;
mod ui_colors;
mod usage;

use anyhow::Result;
use clap::Parser;
use config::ToolSelection;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Error: {}", terminal_text::sanitize(&format!("{error:#}")));
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = cli::Args::parse();
    let mut config = config::Config::load()?;
    if !matches!(
        &args.command,
        Some(cli::Command::Acp { .. } | cli::Command::Login { .. })
    ) {
        config.apply_project_config(&std::env::current_dir()?)?;
    }
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
            cli::Command::Acp { permissions } => {
                acp::run(
                    config,
                    acp::AcpPolicy {
                        restore: acp::SessionRestorePolicy {
                            thinking: args.thinking.is_none(),
                            safety: args.safety.is_none(),
                            tools: !tools_overridden,
                            provider: args.provider.is_none(),
                            model: args.model.is_none(),
                        },
                        permissions: *permissions,
                    },
                )
                .await?;
                return Ok(());
            }
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
        args.provider.is_some(),
        args.model.is_some(),
    )
    .await
}
