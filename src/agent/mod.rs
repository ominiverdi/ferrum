pub mod messages;
pub mod tools;

use crate::{config::Config, context, mcp, providers, session, tools as builtin_tools};
use anyhow::Result;
use rustyline::{DefaultEditor, error::ReadlineError};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

pub async fn run_print(prompt: String, config: &Config) -> Result<()> {
    let mut state = AgentState::new(config)?;
    state.run_turn(prompt, config).await
}

pub async fn run_interactive(config: &mut Config, resume: Option<Option<String>>) -> Result<()> {
    let mut state = match resume {
        Some(path) => AgentState::resume(config, path.map(PathBuf::from))?,
        None => AgentState::new(config)?,
    };
    println!("Ferrum interactive. /help for commands.");

    let mut rl = DefaultEditor::new()?;
    let history = config.config_dir.join("history.txt");
    let _ = rl.load_history(&history);

    let mut last_ctrl_c: Option<Instant> = None;
    loop {
        match rl.readline("ferrum> ") {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(input);
                if input.starts_with('/') {
                    match handle_command(input, config, &mut state) {
                        Ok(CommandAction::Continue) => continue,
                        Ok(CommandAction::Quit) => {
                            let _ = rl.save_history(&history);
                            return Ok(());
                        }
                        Err(error) => {
                            eprintln!("Error: {error}");
                            continue;
                        }
                    }
                }
                state.run_turn(input.to_string(), config).await?;
                println!();
            }
            Err(ReadlineError::Interrupted) => {
                let now = Instant::now();
                if last_ctrl_c
                    .is_some_and(|last| now.duration_since(last) <= Duration::from_millis(900))
                {
                    println!("^C^C");
                    let _ = rl.save_history(&history);
                    return Ok(());
                }
                last_ctrl_c = Some(now);
                println!("^C (press Ctrl+C again to quit)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!();
                let _ = rl.save_history(&history);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

struct AgentState {
    session: session::JsonlSession,
    messages: Vec<messages::Message>,
    cwd: std::path::PathBuf,
    mcp: Option<mcp::McpManager>,
}

impl AgentState {
    fn new(config: &Config) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut messages = Vec::new();
        if let Some(system_context) = context::load_context(&config.config_dir, &cwd)? {
            messages.push(messages::Message::text(
                messages::Role::System,
                system_context,
            ));
        }
        Ok(Self {
            session: session::JsonlSession::create(config.sessions_dir())?,
            messages,
            cwd,
            mcp: None,
        })
    }

    fn resume(config: &Config, path: Option<PathBuf>) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let path = match path {
            Some(path) => path,
            None => session::jsonl::latest_session(&config.sessions_dir())?
                .ok_or_else(|| anyhow::anyhow!("no sessions found"))?,
        };
        let messages = session::jsonl::load_messages(&path)?;
        let count = messages.len();
        println!("resumed {} ({count} messages)", path.display());
        Ok(Self {
            session: session::JsonlSession::open(path)?,
            messages,
            cwd,
            mcp: None,
        })
    }

    async fn run_turn(&mut self, prompt: String, config: &Config) -> Result<()> {
        let stats = self.stats();
        let warn_tokens = config.max_context_tokens.saturating_mul(4) / 5;
        if stats.estimated_tokens >= config.max_context_tokens {
            eprintln!(
                "[session] estimated context {} tokens exceeds limit {}; compacting",
                stats.estimated_tokens, config.max_context_tokens
            );
            self.compact()?;
        } else if stats.estimated_tokens >= warn_tokens {
            eprintln!(
                "[session] estimated context {} tokens is near limit {}",
                stats.estimated_tokens, config.max_context_tokens
            );
        }

        let user = messages::Message::text(messages::Role::User, prompt);
        self.session.append_message(&user)?;
        self.messages.push(user);

        self.ensure_mcp(config).await?;
        let provider = providers::from_config(&config.provider);
        let mut tools = builtin_tools::definitions();
        if let Some(mcp) = &self.mcp {
            tools.extend_from_slice(mcp.definitions());
        }

        for _ in 0..8 {
            let response = provider
                .complete(&config.model, &self.messages, &tools, config.thinking)
                .await?;
            print!("{}", response.text_content());
            io::stdout().flush()?;
            self.session.append_message(&response)?;

            let tool_uses: Vec<_> = response
                .content
                .iter()
                .filter_map(|block| match block {
                    messages::ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();
            self.messages.push(response);

            if tool_uses.is_empty() {
                return Ok(());
            }

            for (id, name, input) in tool_uses {
                eprintln!("\n[tool] {name} {input}");
                let (content, is_error) = match self.execute_tool(&name, &input).await {
                    Ok(output) => (output, false),
                    Err(error) => (error.to_string(), true),
                };
                let result = messages::Message {
                    role: messages::Role::Tool,
                    content: vec![messages::ContentBlock::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error,
                    }],
                };
                self.session.append_message(&result)?;
                self.messages.push(result);
            }
        }

        anyhow::bail!("agent loop exceeded maximum tool iterations")
    }

    async fn ensure_mcp(&mut self, config: &Config) -> Result<()> {
        if self.mcp.is_none() && !config.mcp_servers.is_empty() {
            self.mcp = Some(mcp::McpManager::start(&config.mcp_servers).await?);
        }
        Ok(())
    }

    async fn execute_tool(&mut self, name: &str, input: &serde_json::Value) -> Result<String> {
        if let Some(mcp) = &mut self.mcp {
            if mcp.has_tool(name) {
                return mcp.call(name, input).await;
            }
        }
        builtin_tools::execute(name, input, &self.cwd).await
    }

    fn stats(&self) -> SessionStats {
        let chars = self
            .messages
            .iter()
            .map(|message| message.text_content().chars().count())
            .sum::<usize>();
        let file_bytes = fs::metadata(self.session.path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        SessionStats {
            messages: self.messages.len(),
            chars,
            estimated_tokens: chars.div_ceil(4),
            file_bytes,
        }
    }

    fn compact(&mut self) -> Result<()> {
        let mut summary = String::new();
        for message in self
            .messages
            .iter()
            .filter(|m| !matches!(m.role, messages::Role::System))
        {
            let text = message.text_content();
            if !text.trim().is_empty() {
                summary.push_str(&format!("{:?}: {}\n", message.role, text.trim()));
            }
        }
        self.messages
            .retain(|m| matches!(m.role, messages::Role::System));
        self.session.append_compaction(&summary)?;
        self.messages.push(messages::Message::text(
            messages::Role::System,
            format!("Conversation summary before compaction:\n{}", summary),
        ));
        Ok(())
    }
}

struct SessionStats {
    messages: usize,
    chars: usize,
    estimated_tokens: usize,
    file_bytes: u64,
}

enum CommandAction {
    Continue,
    Quit,
}

fn handle_command(
    input: &str,
    config: &mut Config,
    state: &mut AgentState,
) -> Result<CommandAction> {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or("");
    match command {
        "/quit" | "/exit" => Ok(CommandAction::Quit),
        "/help" => {
            println!("commands:");
            println!("  /quit | /exit          exit");
            println!("  /session              show session path/status/size");
            println!("  /model [name]         show or set model");
            println!("  /provider [name]      show or set provider");
            println!(
                "  /thinking [level]     show or set thinking: off|minimal|low|medium|high|xhigh"
            );
            println!("  /compact              compact current in-memory conversation");
            Ok(CommandAction::Continue)
        }
        "/session" => {
            println!("path: {}", state.session.path().display());
            let stats = state.stats();
            println!("messages: {}", stats.messages);
            println!("chars: {}", stats.chars);
            println!("estimated_tokens: {}", stats.estimated_tokens);
            println!("max_context_tokens: {}", config.max_context_tokens);
            println!("file_bytes: {}", stats.file_bytes);
            println!("model: {}", config.model);
            println!("thinking: {:?}", config.thinking);
            println!("provider: {:?}", config.provider);
            Ok(CommandAction::Continue)
        }
        "/model" => {
            if let Some(model) = parts.next() {
                config.model = model.to_string();
            }
            println!("model: {}", config.model);
            Ok(CommandAction::Continue)
        }
        "/provider" => {
            if let Some(provider) = parts.next() {
                config.set_provider(provider)?;
            }
            println!("provider: {:?}", config.provider);
            Ok(CommandAction::Continue)
        }
        "/thinking" => {
            if let Some(thinking) = parts.next() {
                config.thinking = crate::config::ThinkingLevel::parse(thinking)?;
            }
            println!("thinking: {:?}", config.thinking);
            Ok(CommandAction::Continue)
        }
        "/compact" => {
            state.compact()?;
            println!("conversation compacted and persisted");
            Ok(CommandAction::Continue)
        }
        _ => {
            println!("unknown command: {command}");
            Ok(CommandAction::Continue)
        }
    }
}
