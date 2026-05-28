pub mod messages;
pub mod tools;

use crate::{config::Config, context, mcp, providers, session, tools as builtin_tools};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustyline::{DefaultEditor, error::ReadlineError};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub async fn run_print(prompt: String, images: Vec<String>, config: &Config) -> Result<()> {
    let mut state = AgentState::new(config)?;
    state.attach_images(images)?;
    let (prompt, pasted_images) = extract_pasted_images(&prompt, &state.cwd);
    state.attach_images(pasted_images)?;
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
                let input_with_clipboard_paths = replace_paste_image_triggers(input);
                let input = input_with_clipboard_paths.trim();
                if input.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(input);
                if is_slash_command(input) {
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
                let (prompt, image_paths) = extract_pasted_images(input, &state.cwd);
                if !image_paths.is_empty() {
                    match state.attach_images(image_paths) {
                        Ok(()) => {
                            if prompt.trim().is_empty() {
                                continue;
                            }
                        }
                        Err(error) => {
                            eprintln!("Error: {error}");
                            continue;
                        }
                    }
                }
                match state.run_turn(prompt, config).await {
                    Ok(()) => println!(),
                    Err(error) => {
                        eprintln!("Error: {error}");
                        continue;
                    }
                }
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
    pending_images: Vec<messages::ContentBlock>,
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
            pending_images: Vec::new(),
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
            pending_images: Vec::new(),
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

        let images = std::mem::take(&mut self.pending_images);
        let user = if images.is_empty() {
            messages::Message::text(messages::Role::User, prompt)
        } else {
            messages::Message::with_images(messages::Role::User, prompt, images)
        };
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
            print!("{}", response.display_text());
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

    fn attach_clipboard_image(&mut self) -> Result<()> {
        let image = read_clipboard_image()?;
        preview_attached_image(None, &image);
        self.pending_images.push(image);
        eprintln!("[image] attached clipboard image");
        Ok(())
    }

    fn attach_images(&mut self, specs: Vec<String>) -> Result<()> {
        for spec in specs {
            if spec.starts_with("data:image/") {
                let image = messages::image_from_data_uri(&spec)?;
                preview_attached_image(None, &image);
                self.pending_images.push(image);
                eprintln!("[image] attached pasted image");
                continue;
            }

            let resolved = builtin_tools::path::resolve_to_cwd(&spec, &self.cwd)?;
            let image = messages::image_from_path(&resolved)?;
            preview_attached_image(Some(&resolved), &image);
            self.pending_images.push(image);
            eprintln!("[image] attached {}", resolved.display());
        }
        Ok(())
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

fn replace_paste_image_triggers(input: &str) -> String {
    let mut output = input.to_string();
    for trigger in ["\u{16}", "\u{1b}[118;6u", "[118;6u"] {
        while output.contains(trigger) {
            match save_clipboard_image_to_temp() {
                Ok(path) => {
                    eprintln!("[image] clipboard saved as {}", path.display());
                    output = output.replacen(trigger, &format!(" {} ", path.display()), 1);
                }
                Err(error) => {
                    eprintln!("Error: {error}");
                    output = output.replacen(trigger, " ", 1);
                }
            }
        }
    }
    output
}

fn is_slash_command(input: &str) -> bool {
    matches!(
        input.split_whitespace().next().unwrap_or(""),
        "/quit"
            | "/exit"
            | "/help"
            | "/session"
            | "/model"
            | "/provider"
            | "/thinking"
            | "/image"
            | "/paste-image"
            | "/compact"
    )
}

fn extract_pasted_images(input: &str, cwd: &Path) -> (String, Vec<String>) {
    let mut prompt_parts = Vec::new();
    let mut image_paths = Vec::new();

    for part in input.split_whitespace() {
        let trimmed = part.trim_matches(['\'', '"']);
        if trimmed.starts_with("data:image/") {
            image_paths.push(trimmed.to_string());
        } else if looks_like_image_path(trimmed)
            && builtin_tools::path::resolve_to_cwd(trimmed, cwd).is_ok_and(|path| path.is_file())
        {
            image_paths.push(trimmed.to_string());
        } else {
            prompt_parts.push(part);
        }
    }

    (prompt_parts.join(" "), image_paths)
}

fn looks_like_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.starts_with("file://")
            && (lower.contains(".png")
                || lower.contains(".jpg")
                || lower.contains(".jpeg")
                || lower.contains(".webp"))
}

fn read_clipboard_image() -> Result<messages::ContentBlock> {
    let (mime_type, data) = read_clipboard_image_bytes()?;
    messages::image_from_bytes(mime_type, data, "clipboard".to_string())
}

fn save_clipboard_image_to_temp() -> Result<PathBuf> {
    let (mime_type, data) = read_clipboard_image_bytes()?;
    let image = messages::image_from_bytes(mime_type, data, "clipboard".to_string())?;
    let messages::ContentBlock::Image {
        mime_type,
        data_base64,
        sha256,
        ..
    } = image
    else {
        anyhow::bail!("clipboard did not contain an image")
    };
    let path = std::env::temp_dir().join(format!(
        "ferrum-clipboard-{}.{}",
        &sha256[..12],
        messages::image_extension(&mime_type)
    ));
    let bytes = STANDARD
        .decode(data_base64)
        .context("failed to decode clipboard image")?;
    fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn read_clipboard_image_bytes() -> Result<(String, Vec<u8>)> {
    let x11 =
        std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("x11"));
    let xclip_attempts: &[(&str, &[&str], &str)] = &[
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
            "image/png",
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/jpeg", "-o"],
            "image/jpeg",
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/webp", "-o"],
            "image/webp",
        ),
    ];
    let wayland_attempts: &[(&str, &[&str], &str)] = &[
        (
            "wl-paste",
            &["--no-newline", "--type", "image/png"],
            "image/png",
        ),
        (
            "wl-paste",
            &["--no-newline", "--type", "image/jpeg"],
            "image/jpeg",
        ),
        (
            "wl-paste",
            &["--no-newline", "--type", "image/webp"],
            "image/webp",
        ),
    ];

    let attempts = if x11 {
        [xclip_attempts, wayland_attempts].concat()
    } else {
        [wayland_attempts, xclip_attempts].concat()
    };

    for (command, args, mime_type) in attempts {
        if !command_exists(command) {
            continue;
        }
        let Ok(output) = Command::new(command).args(args).output() else {
            continue;
        };
        if output.status.success() && !output.stdout.is_empty() {
            return Ok((mime_type.to_string(), output.stdout));
        }
    }

    anyhow::bail!(
        "could not read image from clipboard; install wl-clipboard or xclip, or use /image <path>"
    )
}

fn preview_attached_image(path: Option<&Path>, image: &messages::ContentBlock) {
    let messages::ContentBlock::Image {
        mime_type,
        data_base64,
        sha256,
        ..
    } = image
    else {
        return;
    };

    let mut temp_path = None;
    let preview_path = if command_exists("chafa") {
        if let Some(path) = path {
            Some(path.to_path_buf())
        } else {
            temp_path = write_temp_image(image).ok();
            temp_path.clone()
        }
    } else {
        None
    };

    if let Some(path) = preview_path.as_deref() {
        match Command::new("chafa")
            .args(["--size", "80x24"])
            .arg(path)
            .status()
        {
            Ok(status) if status.success() => {
                if let Some(path) = temp_path {
                    let _ = fs::remove_file(path);
                }
                return;
            }
            _ => {}
        }
    }

    if let Some(path) = temp_path {
        let _ = fs::remove_file(path);
    }

    let approx_bytes = data_base64.len().saturating_mul(3) / 4;
    let short_hash = sha256.get(..12).unwrap_or(sha256);
    let source = match image {
        messages::ContentBlock::Image { source, .. } => source.as_str(),
        _ => "image",
    };
    eprintln!("[image] {source} ({mime_type}, ~{approx_bytes} bytes, sha256:{short_hash})");
}

fn write_temp_image(image: &messages::ContentBlock) -> Result<PathBuf> {
    let messages::ContentBlock::Image {
        mime_type,
        data_base64,
        sha256,
        ..
    } = image
    else {
        anyhow::bail!("not an image")
    };
    let ext = messages::image_extension(mime_type);
    let path = std::env::temp_dir().join(format!("ferrum-image-{}.{}", &sha256[..12], ext));
    let data = STANDARD
        .decode(data_base64)
        .context("failed to decode image for preview")?;
    fs::write(&path, data).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
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
            println!("  /image <path>         attach image to next message");
            println!("  /paste-image          attach image from clipboard");
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
            println!("pending_images: {}", state.pending_images.len());
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
        "/image" => {
            let path = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: /image <path>"))?;
            state.attach_images(vec![path.to_string()])?;
            println!("attached image: {path}");
            Ok(CommandAction::Continue)
        }
        "/paste-image" => {
            state.attach_clipboard_image()?;
            println!("attached clipboard image");
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
