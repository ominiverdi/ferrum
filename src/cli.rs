use clap::{Parser, Subcommand};
use std::io::{self, Read};

#[derive(Debug, Parser)]
#[command(name = "ferrum", version, about = "A small Rust-native coding agent")]
pub struct Args {
    /// Run a single prompt and print the answer
    #[arg(short = 'p', long = "print")]
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

    /// Attach a local image file to the prompt. Repeatable. Supports png, jpg, jpeg, webp.
    #[arg(long = "image", value_name = "PATH")]
    pub images: Vec<String>,

    /// Resume the latest session, or a specific JSONL session path
    #[arg(long, value_name = "PATH")]
    pub resume: Option<Option<String>>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Authenticate with a provider
    Login { provider: String },
}

impl Args {
    pub fn print_prompt(&self) -> Option<String> {
        let mut prompt = self.prompt.clone()?;
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
        Some(prompt)
    }
}

fn atty_stdin() -> bool {
    std::io::IsTerminal::is_terminal(&io::stdin())
}
