use clap::{Parser, Subcommand};
use std::io::{self, Read};

#[derive(Debug, Parser)]
#[command(name = "ferrum", version, about = "A small Rust-native coding agent")]
pub struct Args {
    /// Run a single prompt and print the answer. If omitted, read prompt from stdin.
    #[arg(short = 'p', long = "print", num_args = 0..=1, default_missing_value = "")]
    pub prompt: Option<String>,

    /// Override provider name from config.toml
    #[arg(long)]
    pub provider: Option<String>,

    /// Override model name from config.toml
    #[arg(long)]
    pub model: Option<String>,

    /// Override thinking level: off|minimal|low|medium|high|xhigh
    #[arg(long)]
    pub thinking: Option<String>,

    /// Override tool execution safety level: low|medium|high
    #[arg(long)]
    pub safety: Option<String>,

    /// Set the session title
    #[arg(long)]
    pub title: Option<String>,

    /// Attach a local image file to the prompt. Repeatable. Supports png, jpg, jpeg, webp.
    #[arg(long = "image", value_name = "PATH")]
    pub images: Vec<String>,

    /// Enable configured MCP servers for this process. Optionally pass server names.
    #[arg(long = "mcp", num_args = 0.., value_name = "SERVER", conflicts_with = "no_mcp")]
    pub mcp: Option<Vec<String>>,

    /// Disable MCP servers for this process
    #[arg(long = "no-mcp")]
    pub no_mcp: bool,

    /// Disable all tools for this process
    #[arg(long = "no-tools", conflicts_with = "tools")]
    pub no_tools: bool,

    /// Expose only these tools to the model
    #[arg(long = "tools", num_args = 1.., value_name = "TOOL")]
    pub tools: Option<Vec<String>>,

    /// Resume the latest session, or a specific JSONL session path/id prefix
    #[arg(long, value_name = "REF")]
    pub resume: Option<Option<String>>,

    /// Continue the latest session for the current directory
    #[arg(long = "continue")]
    pub r#continue: bool,

    /// Open a specific session by path or id prefix
    #[arg(long, value_name = "REF")]
    pub session: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the official ACP v1 stdio agent
    Acp,
    /// Authenticate with a provider
    Login { provider: String },
}

impl Args {
    pub fn print_prompt(&self) -> anyhow::Result<Option<String>> {
        let Some(mut prompt) = self.prompt.clone() else {
            return Ok(None);
        };
        let mut stdin = String::new();
        if !atty_stdin() {
            let _ = io::stdin().read_to_string(&mut stdin);
        }
        if !stdin.trim().is_empty() {
            if !prompt.is_empty() {
                prompt.push_str("\n\n");
            }
            prompt.push_str(&stdin);
        }
        if prompt.trim().is_empty() {
            anyhow::bail!("print mode requires a prompt argument or stdin input");
        }
        Ok(Some(prompt))
    }
}

fn atty_stdin() -> bool {
    std::io::IsTerminal::is_terminal(&io::stdin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_acp_subcommand() {
        let args = Args::try_parse_from(["ferrum", "acp"]).unwrap();
        assert!(matches!(args.command, Some(Command::Acp)));
    }

    #[test]
    fn parses_mcp_flags() {
        let enabled = Args::try_parse_from(["ferrum", "--mcp", "-p", "hi"]).unwrap();
        assert_eq!(enabled.mcp, Some(Vec::new()));
        assert!(!enabled.no_mcp);

        let filtered = Args::try_parse_from([
            "ferrum",
            "--mcp",
            "chrome-devtools",
            "web-search",
            "-p",
            "hi",
        ])
        .unwrap();
        assert_eq!(
            filtered.mcp,
            Some(vec![
                "chrome-devtools".to_string(),
                "web-search".to_string()
            ])
        );

        let disabled = Args::try_parse_from(["ferrum", "--no-mcp", "-p", "hi"]).unwrap();
        assert_eq!(disabled.mcp, None);
        assert!(disabled.no_mcp);
    }

    #[test]
    fn mcp_flags_conflict() {
        let result = Args::try_parse_from(["ferrum", "--mcp", "--no-mcp", "-p", "hi"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_tools_flag() {
        let args = Args::try_parse_from(["ferrum", "--tools", "read", "grep", "-p", "hi"]).unwrap();
        assert_eq!(
            args.tools,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        assert!(!args.no_tools);

        let none = Args::try_parse_from(["ferrum", "--no-tools", "-p", "hi"]).unwrap();
        assert_eq!(none.tools, None);
        assert!(none.no_tools);
    }

    #[test]
    fn parses_safety_flag() {
        let args = Args::try_parse_from(["ferrum", "--safety", "high", "-p", "hi"]).unwrap();
        assert_eq!(args.safety.as_deref(), Some("high"));
    }

    #[test]
    fn parses_print_without_prompt_value() {
        let args = Args::try_parse_from(["ferrum", "-p"]).unwrap();
        assert_eq!(args.prompt.as_deref(), Some(""));
    }

    #[test]
    fn parses_print_with_prompt_value() {
        let args = Args::try_parse_from(["ferrum", "-p", "hi"]).unwrap();
        assert_eq!(args.prompt.as_deref(), Some("hi"));
    }

    #[test]
    fn parses_title_flag() {
        let args = Args::try_parse_from(["ferrum", "--title", "Issue triage", "-p", "hi"]).unwrap();
        assert_eq!(args.title.as_deref(), Some("Issue triage"));
    }
}
