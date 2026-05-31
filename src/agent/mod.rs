pub mod messages;
pub mod tools;

use crate::{config::Config, context, mcp, providers, session, skills, tools as builtin_tools};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustyline::{DefaultEditor, error::ReadlineError};
use similar::{ChangeTag, TextDiff};
use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

const COMPACTION_KEEP_RECENT_TOKENS: usize = 20_000;
const COMPACTION_TOOL_RESULT_MAX_CHARS: usize = 2_000;
const TOOL_PREVIEW_MAX_CHARS: usize = 4_000;
const HARD_TOOL_ROUND_LIMIT: usize = 256;
const REPEATED_TOOL_NUDGE_LIMIT: usize = 4;
const REPEATED_TOOL_FORCE_LIMIT: usize = 7;
const CONSECUTIVE_ERROR_NUDGE_LIMIT: usize = 5;
const CONSECUTIVE_ERROR_FORCE_LIMIT: usize = 8;

pub async fn run_print(prompt: String, images: Vec<String>, config: &Config) -> Result<()> {
    let mut state = AgentState::new(config)?;
    state.attach_images(images)?;
    let (prompt, pasted_images) = extract_pasted_images(&prompt, &state.cwd);
    state.attach_images(pasted_images)?;
    state.run_turn(prompt, config).await
}

pub async fn run_interactive(
    config: &mut Config,
    resume: Option<Option<String>>,
    continue_latest: bool,
    session_ref: Option<String>,
) -> Result<()> {
    let mut state = match (session_ref, resume, continue_latest) {
        (Some(reference), _, _) => AgentState::resume_ref(config, Some(&reference))?,
        (None, Some(Some(reference)), _) => AgentState::resume_ref(config, Some(&reference))?,
        (None, Some(None), _) | (None, None, true) => AgentState::resume_ref(config, None)?,
        (None, None, false) => AgentState::new(config)?,
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
                if input.starts_with('!') {
                    match handle_bang_command(input, config, &mut state).await {
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
                if let Some((name, args)) = parse_skill_invocation(input) {
                    match state.expand_skill_prompt(name, args.as_deref()) {
                        Ok(prompt) => match state.run_turn(prompt, config).await {
                            Ok(()) => println!(),
                            Err(error) => eprintln!("Error: {error}"),
                        },
                        Err(error) => eprintln!("Error: {error}"),
                    }
                    continue;
                }
                if input == "/models" {
                    match providers::list_models(&config.provider).await {
                        Ok(providers::ModelList::Live { source, models }) => {
                            println!("models from {source}:");
                            for model in models {
                                let marker = if model == config.model { "*" } else { " " };
                                println!("{marker} {model}");
                            }
                        }
                        Err(error) => eprintln!("Error: {error}"),
                    }
                    continue;
                }
                if input == "/compact" || input.starts_with("/compact ") {
                    let instructions = input.strip_prefix("/compact ").map(str::trim);
                    match state.compact(config, instructions, false).await {
                        Ok(CompactionOutcome::Compacted {
                            before_tokens,
                            after_tokens,
                        }) => println!(
                            "conversation compacted: {before_tokens} -> {after_tokens} estimated tokens"
                        ),
                        Ok(CompactionOutcome::Skipped {
                            before_tokens,
                            after_tokens,
                            reason,
                        }) => println!(
                            "compaction skipped: {reason} ({before_tokens} -> {after_tokens} estimated tokens)"
                        ),
                        Err(error) => eprintln!("Error: {error}"),
                    }
                    continue;
                }
                if should_handle_as_command(input, &state.cwd) {
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

fn runtime_context(config: &Config, cwd: &Path) -> String {
    format!(
        "You are running inside Ferrum, a Rust-native Linux coding agent.\n\nRuntime metadata:\n- ferrum_version: {}\n- provider: {}\n- model: {}\n- thinking: {:?}\n- cwd: {}\n- config_dir: {}\n- max_context_tokens: {}\n- max_tool_rounds: {}\n- mcp_enabled: {}\n\nAgent behavior:\n- Be proactive. If the user asks you to investigate local state, use tools before asking for information that Ferrum can inspect.\n- Do not claim you searched something unless a tool result supports it.\n- Prefer targeted evidence over broad noisy scans. Start narrow, then widen deliberately.\n- For Linux desktop/service issues, check likely systemd user units, service files, logs, running processes, executable paths, environment/session type, and relevant config.\n- When using tools, read important files directly and cite exact paths, commands, and error messages.\n- After several tool calls, synthesize what is known, what is still unknown, and the next concrete action. Do not loop indefinitely.\n- If the adaptive loop guard stops tool use, summarize findings from available evidence instead of continuing to search.\n\nTool usage guidance:\n- Use read for known files.\n- Prefer native ls/find/grep for filesystem exploration when they fit. They are safer and avoid noisy dependency/build directories.\n- Avoid broad bash find/grep over \".\" unless needed. If using shell find/grep, prune .git, target, node_modules, and other dependency/build directories.\n- Use bash for shell commands, systemctl, journalctl, process inspection, package checks, and focused pipelines.\n- Keep bash commands focused and safe. Avoid destructive commands unless the user explicitly asked for them.\n- For long-running or background scripts, use nohup with redirected logs and verify separately.\n\nInteractive commands available to the user:\n- /help\n- /version\n- /session\n- /sessions\n- /sessions <number|id-prefix|path>\n- /sessions pick\n- /sessions new\n- /model [name]\n- /models\n- /provider [name]\n- /providers\n- /mcp [on|off|status]\n- /thinking [off|minimal|low|medium|high|xhigh]\n- /skills\n- /skill:<name> [args]\n- /image <path>\n- /paste-image\n- /compact\n- /quit\n\nShell shortcuts available to the user:\n- !<cmd>: run a shell command and send its output to the model\n- !!<cmd>: run a shell command and show output only to the user\n\nThese slash commands and shell shortcuts are handled by Ferrum before user messages are sent to you. You cannot execute them by printing them; tell the user which command to run when needed.",
        env!("CARGO_PKG_VERSION"),
        config.provider_name,
        config.model,
        config.thinking,
        cwd.display(),
        config.config_dir.display(),
        config.max_context_tokens,
        config.max_tool_rounds,
        config.mcp_enabled,
    )
}

#[derive(Debug)]
struct ExecutedToolUse {
    id: String,
    name: String,
    input: serde_json::Value,
    content: String,
    is_error: bool,
    duration_ms: u128,
}

#[derive(Debug)]
struct ToolObservation {
    fingerprint: String,
    is_error: bool,
}

impl ToolObservation {
    fn new(name: &str, input: &serde_json::Value, is_error: bool) -> Self {
        let input = serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
        Self {
            fingerprint: format!("{name}:{input}"),
            is_error,
        }
    }
}

fn metrics_enabled() -> bool {
    matches!(
        std::env::var("FERRUM_METRICS").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn emit_model_metrics_start(
    request: usize,
    messages: &[messages::Message],
    tools: &[tools::ToolDefinition],
) {
    let text_chars = messages
        .iter()
        .map(|message| message.text_content().chars().count())
        .sum::<usize>();
    let message_bytes = serde_json::to_vec(messages)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    let tool_schema_bytes = serde_json::to_vec(tools)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    let payload_bytes = message_bytes.saturating_add(tool_schema_bytes);
    eprintln!(
        "[metrics:model start] request={request} messages={} text_chars={text_chars} text_estimated_tokens={} message_bytes={message_bytes} tools={} tool_schema_bytes={tool_schema_bytes} payload_bytes={payload_bytes} payload_estimated_tokens={}",
        messages.len(),
        text_chars.div_ceil(4),
        tools.len(),
        payload_bytes.div_ceil(4)
    );
}

fn emit_model_metrics_end(request: usize, duration: Duration, response: &messages::Message) {
    let output_chars = response.text_content().chars().count();
    let tool_calls = response
        .content
        .iter()
        .filter(|block| matches!(block, messages::ContentBlock::ToolUse { .. }))
        .count();
    eprintln!(
        "[metrics:model end] request={request} latency_ms={} output_chars={output_chars} output_estimated_tokens={} tool_calls={tool_calls}",
        duration.as_millis(),
        output_chars.div_ceil(4)
    );
}

fn emit_tool_metrics_if_enabled(result: &ExecutedToolUse) {
    if !metrics_enabled() {
        return;
    }
    eprintln!(
        "[metrics:tool] name={} latency_ms={} result_bytes={} is_error={}",
        result.name,
        result.duration_ms,
        result.content.len(),
        result.is_error
    );
}

fn tool_schema_bytes(tools: &[tools::ToolDefinition]) -> usize {
    serde_json::to_vec(tools)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

#[derive(Debug, PartialEq, Eq)]
enum LoopGuardAction {
    Continue,
    Nudge(String),
    ForceFinal(String),
}

#[derive(Debug)]
struct LoopGuard {
    explicit_limit: usize,
    rounds: usize,
    consecutive_errors: usize,
    repeated_tool_calls: HashMap<String, usize>,
    repeated_nudged: bool,
    errors_nudged: bool,
}

impl LoopGuard {
    fn new(explicit_limit: usize) -> Self {
        Self {
            explicit_limit,
            rounds: 0,
            consecutive_errors: 0,
            repeated_tool_calls: HashMap::new(),
            repeated_nudged: false,
            errors_nudged: false,
        }
    }

    fn observe_round(&mut self, observations: &[ToolObservation]) -> LoopGuardAction {
        self.rounds += 1;
        if self.explicit_limit > 0 && self.rounds >= self.explicit_limit {
            return LoopGuardAction::ForceFinal(format!(
                "explicit tool round limit ({}) reached",
                self.explicit_limit
            ));
        }
        if self.rounds >= HARD_TOOL_ROUND_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "hard safety limit ({HARD_TOOL_ROUND_LIMIT}) reached"
            ));
        }

        let mut max_repeats = 0;
        let mut repeated_fingerprint = None;
        for observation in observations {
            let count = self
                .repeated_tool_calls
                .entry(observation.fingerprint.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            if *count > max_repeats {
                max_repeats = *count;
                repeated_fingerprint = Some(observation.fingerprint.as_str());
            }

            if observation.is_error {
                self.consecutive_errors += 1;
            } else {
                self.consecutive_errors = 0;
            }
        }

        if max_repeats >= REPEATED_TOOL_FORCE_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "same tool call repeated {max_repeats} times ({})",
                repeated_fingerprint.unwrap_or("unknown")
            ));
        }
        if max_repeats >= REPEATED_TOOL_NUDGE_LIMIT && !self.repeated_nudged {
            self.repeated_nudged = true;
            return LoopGuardAction::Nudge(format!(
                "same tool call repeated {max_repeats} times ({})",
                repeated_fingerprint.unwrap_or("unknown")
            ));
        }

        if self.consecutive_errors >= CONSECUTIVE_ERROR_FORCE_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "{} consecutive tool errors",
                self.consecutive_errors
            ));
        }
        if self.consecutive_errors >= CONSECUTIVE_ERROR_NUDGE_LIMIT && !self.errors_nudged {
            self.errors_nudged = true;
            return LoopGuardAction::Nudge(format!(
                "{} consecutive tool errors",
                self.consecutive_errors
            ));
        }

        LoopGuardAction::Continue
    }
}

#[cfg(test)]
mod loop_guard_tests {
    use super::*;

    fn observation(name: &str, input: serde_json::Value, is_error: bool) -> ToolObservation {
        ToolObservation::new(name, &input, is_error)
    }

    #[test]
    fn nudges_then_forces_repeated_tool_calls() {
        let mut guard = LoopGuard::new(0);
        let read = observation("read", serde_json::json!({"path": "a.txt"}), false);
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert!(matches!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Nudge(reason) if reason.contains("same tool call repeated")
        ));
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert!(matches!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::ForceFinal(reason) if reason.contains("same tool call repeated")
        ));
    }

    #[test]
    fn nudges_consecutive_tool_errors() {
        let mut guard = LoopGuard::new(0);
        for index in 0..4 {
            let failed = observation(
                "edit",
                serde_json::json!({"path": format!("{index}.txt")}),
                true,
            );
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&failed)),
                LoopGuardAction::Continue
            );
        }
        let failed = observation("edit", serde_json::json!({"path": "final.txt"}), true);
        assert!(matches!(
            guard.observe_round(std::slice::from_ref(&failed)),
            LoopGuardAction::Nudge(reason) if reason.contains("consecutive tool errors")
        ));
    }

    #[test]
    fn explicit_limit_forces_final() {
        let mut guard = LoopGuard::new(2);
        let read = observation("read", serde_json::json!({"path": "a.txt"}), false);
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert!(matches!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::ForceFinal(reason) if reason.contains("explicit tool round limit")
        ));
    }
}

struct AgentState {
    session: session::JsonlSession,
    messages: Vec<messages::Message>,
    skills: Vec<skills::Skill>,
    cwd: std::path::PathBuf,
    mcp: Option<mcp::McpManager>,
    mcp_enabled: bool,
    pending_images: Vec<messages::ContentBlock>,
    last_session_list: Vec<session::jsonl::SessionInfo>,
}

impl AgentState {
    fn new(config: &Config) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut messages = Vec::new();
        messages.push(messages::Message::text(
            messages::Role::System,
            runtime_context(config, &cwd),
        ));
        if let Some(system_context) = context::load_context(&config.config_dir, &cwd)? {
            messages.push(messages::Message::text(
                messages::Role::System,
                system_context,
            ));
        }
        let skills = skills::discover(&config.config_dir, &cwd)?;
        if let Some(skill_context) = skills::render_available_skills(&skills) {
            messages.push(messages::Message::text(
                messages::Role::System,
                skill_context,
            ));
        }
        Ok(Self {
            session: session::JsonlSession::create(
                config.sessions_dir(),
                Some(config.provider_name.clone()),
                Some(config.model.clone()),
            )?,
            messages,
            skills,
            cwd,
            mcp: None,
            mcp_enabled: config.mcp_enabled,
            pending_images: Vec::new(),
            last_session_list: Vec::new(),
        })
    }

    fn resume_ref(config: &Config, reference: Option<&str>) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let path = match reference {
            Some(reference) => {
                session::jsonl::resolve_session_ref(&config.sessions_dir(), &cwd, reference)?
            }
            None => session::jsonl::latest_session_for_cwd(&config.sessions_dir(), &cwd)?
                .ok_or_else(|| anyhow::anyhow!("no sessions found for {}", cwd.display()))?,
        };
        Self::open_session(config, path)
    }

    fn open_session(config: &Config, path: PathBuf) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut messages = session::jsonl::load_messages(&path)?;
        let count = messages.len();
        println!("resumed {} ({count} messages)", path.display());
        messages.push(messages::Message::text(
            messages::Role::System,
            runtime_context(config, &cwd),
        ));
        if let Some(system_context) = context::load_context(&config.config_dir, &cwd)? {
            messages.push(messages::Message::text(
                messages::Role::System,
                system_context,
            ));
        }
        let skills = skills::discover(&config.config_dir, &cwd)?;
        if let Some(skill_context) = skills::render_available_skills(&skills) {
            messages.push(messages::Message::text(
                messages::Role::System,
                skill_context,
            ));
        }
        Ok(Self {
            session: session::JsonlSession::open(path)?,
            messages,
            skills,
            cwd,
            mcp: None,
            mcp_enabled: config.mcp_enabled,
            pending_images: Vec::new(),
            last_session_list: Vec::new(),
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
            let outcome = self.compact(config, None, true).await?;
            match outcome {
                CompactionOutcome::Compacted {
                    before_tokens,
                    after_tokens,
                } => eprintln!(
                    "[session] compacted context: {before_tokens} -> {after_tokens} estimated tokens"
                ),
                CompactionOutcome::Skipped { reason, .. } => {
                    eprintln!("[session] compaction skipped: {reason}")
                }
            }
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

        let provider = providers::from_config(&config.provider);
        let mut tools = builtin_tools::definitions();
        if self.mcp_enabled {
            self.ensure_mcp(config).await?;
            if let Some(mcp) = &self.mcp {
                tools.extend_from_slice(mcp.definitions());
            }
        }

        let metrics_enabled = metrics_enabled();
        let mut model_request_index = 0usize;
        let mut loop_guard = LoopGuard::new(config.max_tool_rounds);
        let force_final_reason = loop {
            model_request_index += 1;
            if metrics_enabled {
                emit_model_metrics_start(model_request_index, &self.messages, &tools);
            }
            let started = Instant::now();
            let response = provider
                .complete(&config.model, &self.messages, &tools, config.thinking)
                .await?;
            if metrics_enabled {
                emit_model_metrics_end(model_request_index, started.elapsed(), &response);
            }
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

            let executed_tools = self.execute_tool_batch(tool_uses).await;
            let mut observations = Vec::new();
            for executed in executed_tools {
                observations.push(ToolObservation::new(
                    &executed.name,
                    &executed.input,
                    executed.is_error,
                ));
                let result = messages::Message {
                    role: messages::Role::Tool,
                    content: vec![messages::ContentBlock::ToolResult {
                        tool_use_id: executed.id,
                        content: executed.content,
                        is_error: executed.is_error,
                    }],
                };
                self.session.append_message(&result)?;
                self.messages.push(result);
            }

            match loop_guard.observe_round(&observations) {
                LoopGuardAction::Continue => {}
                LoopGuardAction::Nudge(reason) => {
                    let message = messages::Message::text(
                        messages::Role::System,
                        format!(
                            "Adaptive loop guard: {reason}. Do not repeat the same failed or redundant action. Use existing tool results, choose a different concrete action, or finish with a concise summary if enough evidence is available."
                        ),
                    );
                    eprintln!("[loop-guard] {reason}");
                    self.session.append_message(&message)?;
                    self.messages.push(message);
                }
                LoopGuardAction::ForceFinal(reason) => break reason,
            }
        };

        eprintln!("[loop-guard] stopped tool use: {force_final_reason}");
        let mut final_messages = self.messages.clone();
        final_messages.push(messages::Message::text(
            messages::Role::System,
            format!(
                "Adaptive loop guard stopped tool use: {force_final_reason}. Do not call tools. Summarize the findings from the available tool results, identify likely conclusions, and propose the next concrete step."
            ),
        ));
        model_request_index += 1;
        if metrics_enabled {
            emit_model_metrics_start(model_request_index, &final_messages, &[]);
        }
        let started = Instant::now();
        let final_response = provider
            .complete(&config.model, &final_messages, &[], config.thinking)
            .await?;
        if metrics_enabled {
            emit_model_metrics_end(model_request_index, started.elapsed(), &final_response);
        }
        print!("{}", final_response.display_text());
        io::stdout().flush()?;
        self.session.append_message(&final_response)?;
        self.messages.push(final_response);
        Ok(())
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
        if self.mcp_enabled && self.mcp.is_none() && !config.mcp_servers.is_empty() {
            self.mcp = Some(mcp::McpManager::start(&config.mcp_servers).await?);
        }
        Ok(())
    }

    fn set_mcp_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.mcp_enabled == enabled {
            println!("MCP: {}", if enabled { "on" } else { "off" });
            return Ok(());
        }
        self.mcp_enabled = enabled;
        let message = messages::Message::text(
            messages::Role::System,
            format!(
                "Runtime MCP availability changed. MCP tools are now {} for future turns.",
                if enabled { "enabled" } else { "disabled" }
            ),
        );
        self.session.append_message(&message)?;
        self.messages.push(message);
        println!("MCP: {}", if enabled { "on" } else { "off" });
        Ok(())
    }

    fn print_mcp_status(&self, config: &Config) {
        let native_tools = builtin_tools::definitions();
        let mcp_tools = if self.mcp_enabled {
            self.mcp
                .as_ref()
                .map(|mcp| mcp.definitions())
                .unwrap_or(&[])
        } else {
            &[]
        };
        let mut exposed = native_tools.clone();
        exposed.extend_from_slice(mcp_tools);
        let configured = config.mcp_servers.len();
        let configured_enabled = config
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .count();
        println!("MCP: {}", if self.mcp_enabled { "on" } else { "off" });
        println!("configured_servers: {configured}");
        println!("configured_enabled_servers: {configured_enabled}");
        println!("connected: {}", self.mcp.is_some());
        println!("native_tools: {}", native_tools.len());
        println!("mcp_tools_exposed: {}", mcp_tools.len());
        println!("total_tools_exposed: {}", exposed.len());
        println!("tool_schema_bytes: {}", tool_schema_bytes(&exposed));
        if !config.mcp_servers.is_empty() {
            println!("servers:");
            for server in &config.mcp_servers {
                println!("- {} enabled={}", server.name, server.enabled);
            }
        }
    }

    async fn execute_tool_batch(
        &mut self,
        tool_uses: Vec<(String, String, serde_json::Value)>,
    ) -> Vec<ExecutedToolUse> {
        let can_parallelize = tool_uses
            .iter()
            .all(|(_, name, _)| self.is_parallel_safe_builtin_tool(name));
        if can_parallelize && tool_uses.len() > 1 {
            return self.execute_parallel_builtin_tools(tool_uses).await;
        }
        self.execute_sequential_tools(tool_uses).await
    }

    fn is_parallel_safe_builtin_tool(&self, name: &str) -> bool {
        if self.mcp.as_ref().is_some_and(|mcp| mcp.has_tool(name)) {
            return false;
        }
        matches!(name, "read" | "ls" | "grep" | "find")
    }

    async fn execute_parallel_builtin_tools(
        &self,
        tool_uses: Vec<(String, String, serde_json::Value)>,
    ) -> Vec<ExecutedToolUse> {
        for (_, name, input) in &tool_uses {
            eprintln!();
            render_tool_call(name, input);
        }

        let cwd = self.cwd.clone();
        let mut handles = Vec::new();
        for (index, (id, name, input)) in tool_uses.into_iter().enumerate() {
            let cwd = cwd.clone();
            handles.push(tokio::spawn(async move {
                let started = Instant::now();
                let (content, is_error) = match builtin_tools::execute(&name, &input, &cwd).await {
                    Ok(output) => (output, false),
                    Err(error) => (error.to_string(), true),
                };
                (
                    index,
                    ExecutedToolUse {
                        id,
                        name,
                        input,
                        content,
                        is_error,
                        duration_ms: started.elapsed().as_millis(),
                    },
                )
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(error) => results.push((
                    usize::MAX,
                    ExecutedToolUse {
                        id: String::new(),
                        name: "internal".to_string(),
                        input: serde_json::Value::Null,
                        content: format!("parallel tool task failed: {error}"),
                        is_error: true,
                        duration_ms: 0,
                    },
                )),
            }
        }
        results.sort_by_key(|(index, _)| *index);
        let mut executed = Vec::new();
        for (_, result) in results {
            render_tool_result(&result.name, &result.content, result.is_error);
            emit_tool_metrics_if_enabled(&result);
            executed.push(result);
        }
        executed
    }

    async fn execute_sequential_tools(
        &mut self,
        tool_uses: Vec<(String, String, serde_json::Value)>,
    ) -> Vec<ExecutedToolUse> {
        let mut results = Vec::new();
        for (id, name, input) in tool_uses {
            eprintln!();
            render_tool_call(&name, &input);
            let started = Instant::now();
            let (content, is_error) = match self.execute_tool(&name, &input).await {
                Ok(output) => (output, false),
                Err(error) => (error.to_string(), true),
            };
            render_tool_result(&name, &content, is_error);
            let result = ExecutedToolUse {
                id,
                name,
                input,
                content,
                is_error,
                duration_ms: started.elapsed().as_millis(),
            };
            emit_tool_metrics_if_enabled(&result);
            results.push(result);
        }
        results
    }

    async fn execute_tool(&mut self, name: &str, input: &serde_json::Value) -> Result<String> {
        if self.mcp_enabled {
            if let Some(mcp) = &mut self.mcp {
                if mcp.has_tool(name) {
                    return mcp.call(name, input).await;
                }
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

    fn list_sessions(&mut self, config: &Config) -> Result<()> {
        let sessions = session::jsonl::list_sessions_for_cwd(&config.sessions_dir(), &self.cwd)?;
        self.last_session_list = sessions.clone();
        if sessions.is_empty() {
            println!("No sessions found in {}", self.cwd.display());
            return Ok(());
        }
        println!("Recent sessions in {}\n", self.cwd.display());
        print_session_list(&sessions, self.session.path());
        println!("\nUse /sessions 2, /sessions pick, or /sessions new");
        Ok(())
    }

    fn open_session_reference(&mut self, config: &Config, reference: &str) -> Result<()> {
        let path = self.resolve_session_reference(config, reference)?;
        if path == *self.session.path() {
            println!("Already on session {}", path.display());
            return Ok(());
        }
        let next = Self::open_session(config, path)?;
        *self = next;
        Ok(())
    }

    fn resolve_session_reference(&self, config: &Config, reference: &str) -> Result<PathBuf> {
        if let Ok(index) = reference.parse::<usize>() {
            if self.last_session_list.is_empty() {
                anyhow::bail!(
                    "no session list is active. Run /sessions first, then use /sessions 1"
                )
            }
            let Some(session) = self.last_session_list.get(index.saturating_sub(1)) else {
                anyhow::bail!("no session [{index}] in the last /sessions list")
            };
            return Ok(session.path.clone());
        }
        session::jsonl::resolve_session_ref(&config.sessions_dir(), &self.cwd, reference)
    }

    fn new_session(&mut self, config: &Config) -> Result<()> {
        let next = Self::new(config)?;
        println!("started new session {}", next.session.path().display());
        *self = next;
        Ok(())
    }

    fn pick_session(&mut self, config: &Config) -> Result<()> {
        let mut query = String::new();
        loop {
            let all = session::jsonl::list_sessions_for_cwd(&config.sessions_dir(), &self.cwd)?;
            let filtered = filter_sessions(&all, &query);
            self.last_session_list = filtered.clone();
            if query.is_empty() {
                println!("Recent sessions in {}\n", self.cwd.display());
            } else {
                println!("Recent sessions matching '{query}'\n");
            }
            if filtered.is_empty() {
                println!("No matching sessions");
            } else {
                print_session_list(&filtered, self.session.path());
            }
            print!("\nOpen number, search text, or blank to cancel: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();
            if input.is_empty() {
                println!("cancelled");
                return Ok(());
            }
            if input.chars().all(|ch| ch.is_ascii_digit()) {
                return self.open_session_reference(config, input);
            }
            query = input.to_string();
        }
    }

    fn expand_skill_prompt(&self, name: &str, args: Option<&str>) -> Result<String> {
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown skill: {name}"))?;
        skills::expand_skill_prompt(skill, args)
    }

    async fn compact(
        &mut self,
        config: &Config,
        custom_instructions: Option<&str>,
        force: bool,
    ) -> Result<CompactionOutcome> {
        let before_tokens = estimated_tokens_for_messages(&self.messages);
        let (system_messages, conversation): (Vec<_>, Vec<_>) = self
            .messages
            .iter()
            .cloned()
            .partition(|message| matches!(message.role, messages::Role::System));

        if conversation.is_empty() {
            return Ok(CompactionOutcome::Skipped {
                before_tokens,
                after_tokens: before_tokens,
                reason: "no conversation messages to compact".to_string(),
            });
        }

        let keep_recent_tokens = COMPACTION_KEEP_RECENT_TOKENS.min(config.max_context_tokens / 2);
        let split_index = split_for_compaction(&conversation, keep_recent_tokens.max(1));
        let (to_summarize, recent) = conversation.split_at(split_index);
        if to_summarize.is_empty() {
            return Ok(CompactionOutcome::Skipped {
                before_tokens,
                after_tokens: before_tokens,
                reason: "conversation is already within recent-context budget".to_string(),
            });
        }

        let summary = match self
            .generate_compaction_summary(config, to_summarize, custom_instructions)
            .await
        {
            Ok(summary) if !summary.trim().is_empty() => summary,
            Ok(_) => anyhow::bail!("compaction summary was empty"),
            Err(error) if force => {
                eprintln!(
                    "[session] model compaction failed: {error}; using local fallback summary"
                );
                local_compaction_summary(to_summarize, custom_instructions)
            }
            Err(error) => return Err(error).context("model compaction failed"),
        };

        let summary_message = messages::Message::text(
            messages::Role::System,
            format!(
                "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{}\n</summary>",
                summary.trim()
            ),
        );

        let mut compacted_messages = system_messages;
        compacted_messages.push(summary_message.clone());
        compacted_messages.extend(recent.iter().cloned());
        let after_tokens = estimated_tokens_for_messages(&compacted_messages);

        if !force && after_tokens >= before_tokens {
            return Ok(CompactionOutcome::Skipped {
                before_tokens,
                after_tokens,
                reason: "summary would not reduce context".to_string(),
            });
        }

        self.session.append_compaction(summary.trim())?;
        self.messages = compacted_messages;
        Ok(CompactionOutcome::Compacted {
            before_tokens,
            after_tokens,
        })
    }

    async fn generate_compaction_summary(
        &self,
        config: &Config,
        messages: &[messages::Message],
        custom_instructions: Option<&str>,
    ) -> Result<String> {
        let provider = providers::from_config(&config.provider);
        let prompt = compaction_prompt(messages, custom_instructions);
        let request_messages = vec![
            messages::Message::text(
                messages::Role::System,
                "You are a context summarization assistant. Read the conversation transcript and produce only the requested structured summary. Do not continue the conversation.",
            ),
            messages::Message::text(messages::Role::User, prompt),
        ];
        let response = provider
            .complete(&config.model, &request_messages, &[], config.thinking)
            .await?;
        Ok(response.text_content())
    }
}

fn render_tool_call(name: &str, input: &serde_json::Value) {
    eprintln!("[tool:{name}]");
    match name {
        "bash" => {
            if let Some(command) = input.get("command").and_then(|value| value.as_str()) {
                eprintln!("command:");
                for line in command.lines() {
                    eprintln!("  {line}");
                }
            }
            if let Some(timeout) = input
                .get("timeout_seconds")
                .and_then(|value| value.as_u64())
            {
                eprintln!("timeout: {timeout}s");
            }
        }
        "read" => {
            eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
            if let Some(offset) = input.get("offset").and_then(|value| value.as_u64()) {
                eprintln!("offset: {offset}");
            }
            if let Some(limit) = input.get("limit").and_then(|value| value.as_u64()) {
                eprintln!("limit: {limit}");
            }
        }
        "ls" => {
            eprintln!("path: {}", json_str(input, "path").unwrap_or("."));
            if let Some(limit) = input.get("limit").and_then(|value| value.as_u64()) {
                eprintln!("limit: {limit}");
            }
        }
        "grep" => {
            eprintln!(
                "pattern: {}",
                json_str(input, "pattern").unwrap_or("<missing>")
            );
            eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
            if let Some(glob) = json_str(input, "glob") {
                eprintln!("glob: {glob}");
            }
            if let Some(ignore_case) = input.get("ignore_case").and_then(|value| value.as_bool()) {
                eprintln!("ignore_case: {ignore_case}");
            }
            if let Some(literal) = input.get("literal").and_then(|value| value.as_bool()) {
                eprintln!("literal: {literal}");
            }
            if let Some(context) = input.get("context").and_then(|value| value.as_u64()) {
                eprintln!("context: {context}");
            }
            if let Some(limit) = input.get("limit").and_then(|value| value.as_u64()) {
                eprintln!("limit: {limit}");
            }
        }
        "find" => {
            eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
            if let Some(pattern) = json_str(input, "pattern") {
                eprintln!("pattern: {pattern}");
            }
            if let Some(name) = json_str(input, "name") {
                eprintln!("name: {name}");
            }
            if let Some(extension) = json_str(input, "extension") {
                eprintln!("extension: {extension}");
            }
            if let Some(limit) = input.get("limit").and_then(|value| value.as_u64()) {
                eprintln!("limit: {limit}");
            }
        }
        "write" => {
            eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
            if let Some(content) = json_str(input, "content") {
                eprintln!(
                    "content: {} lines, {} bytes",
                    content.lines().count(),
                    content.len()
                );
                let preview = truncate_chars(content, TOOL_PREVIEW_MAX_CHARS);
                if !preview.is_empty() {
                    eprintln!("preview:\n{}", indent_block(&preview));
                    if content.chars().count() > TOOL_PREVIEW_MAX_CHARS {
                        eprintln!("  [content truncated for display]");
                    }
                }
            }
        }
        "edit" => render_edit_call(input),
        _ => {
            let rendered =
                serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
            eprintln!("args:\n{}", indent_block(&rendered));
        }
    }
}

fn render_edit_call(input: &serde_json::Value) {
    eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
    let Some(edits) = input.get("edits").and_then(|value| value.as_array()) else {
        let rendered = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
        eprintln!("args:\n{}", indent_block(&rendered));
        return;
    };

    eprintln!("edits: {}", edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let old_text = json_str(edit, "old_text").unwrap_or("");
        let new_text = json_str(edit, "new_text").unwrap_or("");
        eprintln!();
        eprintln!("edit {}:", index + 1);
        render_text_diff(old_text, new_text);
    }
}

fn render_text_diff(old_text: &str, new_text: &str) {
    eprintln!("--- old");
    eprintln!("+++ new");
    let diff = TextDiff::from_lines(old_text, new_text);
    for group in diff.grouped_ops(3) {
        let old_start = group
            .first()
            .map(|op| op.old_range().start + 1)
            .unwrap_or(1);
        let new_start = group
            .first()
            .map(|op| op.new_range().start + 1)
            .unwrap_or(1);
        eprintln!("@@ -{old_start} +{new_start} @@");
        for op in group {
            for change in diff.iter_changes(&op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                let text = change.to_string();
                if text.ends_with('\n') {
                    eprint!("{prefix}{text}");
                } else {
                    eprintln!("{prefix}{text}");
                    eprintln!("\\ No newline at end of line");
                }
            }
        }
    }
}

fn render_tool_result(name: &str, content: &str, is_error: bool) {
    let status = if is_error { "error" } else { "ok" };
    let line_count = content.lines().count();
    let bytes = content.len();
    eprintln!("[result:{name} {status}, {line_count} lines, {bytes} bytes]");
    let preview = truncate_chars(content.trim(), TOOL_PREVIEW_MAX_CHARS);
    if !preview.is_empty() {
        eprintln!("{}", indent_block(&preview));
        if content.chars().count() > TOOL_PREVIEW_MAX_CHARS {
            eprintln!("  [result truncated for display; full result kept in context]");
        }
    }
}

fn json_str<'a>(input: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|value| value.as_str())
}

fn indent_block(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_session_list(sessions: &[session::jsonl::SessionInfo], current_path: &Path) {
    for (index, session) in sessions.iter().enumerate() {
        let marker = if session.path == current_path {
            "*"
        } else {
            " "
        };
        let age = format_age(session.modified);
        let model = session.model.as_deref().unwrap_or("unknown-model");
        let provider = session.provider.as_deref().unwrap_or("unknown-provider");
        let provider_model = format!("{provider}/{model}");
        println!(
            "[{}] {marker} {:>4} {:>4} msgs  {:<28} {}",
            index + 1,
            age,
            session.message_count,
            truncate_chars(&provider_model, 28).replace('\n', " "),
            session.title
        );
    }
}

fn filter_sessions(
    sessions: &[session::jsonl::SessionInfo],
    query: &str,
) -> Vec<session::jsonl::SessionInfo> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return sessions.to_vec();
    }
    sessions
        .iter()
        .filter(|session| {
            session.title.to_lowercase().contains(&query)
                || session.short_id.to_lowercase().contains(&query)
                || session
                    .model
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        })
        .cloned()
        .collect()
}

fn format_age(modified: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    let minutes = elapsed.as_secs() / 60;
    if minutes < 1 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 60 * 24 {
        format!("{}h", minutes / 60)
    } else if minutes < 60 * 24 * 7 {
        format!("{}d", minutes / (60 * 24))
    } else {
        format!("{}w", minutes / (60 * 24 * 7))
    }
}

#[derive(Debug)]
enum CompactionOutcome {
    Compacted {
        before_tokens: usize,
        after_tokens: usize,
    },
    Skipped {
        before_tokens: usize,
        after_tokens: usize,
        reason: String,
    },
}

fn split_for_compaction(messages: &[messages::Message], keep_recent_tokens: usize) -> usize {
    let mut accumulated = 0usize;
    for (index, message) in messages.iter().enumerate().rev() {
        accumulated = accumulated.saturating_add(estimated_tokens_for_message(message));
        if accumulated >= keep_recent_tokens {
            return index;
        }
    }
    0
}

fn estimated_tokens_for_messages(messages: &[messages::Message]) -> usize {
    messages
        .iter()
        .map(estimated_tokens_for_message)
        .sum::<usize>()
}

fn estimated_tokens_for_message(message: &messages::Message) -> usize {
    message_text_for_compaction(message)
        .chars()
        .count()
        .div_ceil(4)
}

fn compaction_prompt(messages: &[messages::Message], custom_instructions: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("<conversation>\n");
    for message in messages {
        let text = message_text_for_compaction(message);
        if !text.trim().is_empty() {
            let _ = writeln!(prompt, "{}\n", text.trim());
        }
    }
    prompt.push_str("</conversation>\n\n");
    prompt.push_str(
        "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another coding assistant will use to continue the work.\n\n\
Use this EXACT format:\n\n\
## Goal\n\
[What is the user trying to accomplish?]\n\n\
## Constraints & Preferences\n\
- [Constraints, preferences, requirements, or \"(none)\"]\n\n\
## Progress\n\
### Done\n\
- [x] [Completed tasks/changes]\n\n\
### In Progress\n\
- [ ] [Current work]\n\n\
### Blocked\n\
- [Issues preventing progress, or \"(none)\"]\n\n\
## Key Decisions\n\
- **[Decision]**: [Brief rationale]\n\n\
## Next Steps\n\
1. [Ordered list of what should happen next]\n\n\
## Critical Context\n\
- [Exact file paths, commands, errors, provider/model details, or \"(none)\"]\n\n\
Keep each section concise. Preserve exact file paths, function names, commands, and error messages.",
    );
    if let Some(instructions) = custom_instructions.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nAdditional focus:\n");
        prompt.push_str(instructions.trim());
    }
    prompt
}

fn local_compaction_summary(
    messages: &[messages::Message],
    custom_instructions: Option<&str>,
) -> String {
    let mut summary = String::new();
    summary.push_str("## Goal\n(unknown; model summarization failed)\n\n");
    summary.push_str("## Constraints & Preferences\n- (unknown)\n\n");
    summary.push_str("## Progress\n### Done\n");
    for message in messages {
        let text = message_text_for_compaction(message);
        if !text.trim().is_empty() {
            let _ = writeln!(
                summary,
                "- {:?}: {}",
                message.role,
                truncate_chars(text.trim(), 500)
            );
        }
    }
    summary.push_str("\n### In Progress\n- (unknown)\n\n");
    summary.push_str("### Blocked\n- model-generated compaction failed\n\n");
    summary.push_str("## Key Decisions\n- (unknown)\n\n");
    summary.push_str("## Next Steps\n1. Continue from the retained recent conversation.\n\n");
    summary.push_str("## Critical Context\n- This is a local fallback summary.\n");
    if let Some(instructions) = custom_instructions.filter(|value| !value.trim().is_empty()) {
        summary.push_str("- User compaction focus: ");
        summary.push_str(instructions.trim());
        summary.push('\n');
    }
    summary
}

fn message_text_for_compaction(message: &messages::Message) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "[{:?}]", message.role);
    for block in &message.content {
        match block {
            messages::ContentBlock::Text { text } if !text.trim().is_empty() => {
                let _ = writeln!(output, "{}", text.trim());
            }
            messages::ContentBlock::ToolUse { name, input, .. } => {
                let _ = writeln!(output, "tool_call: {name} {input}");
            }
            messages::ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                let label = if *is_error {
                    "tool_error"
                } else {
                    "tool_result"
                };
                let _ = writeln!(
                    output,
                    "{label}: {}",
                    truncate_chars(content.trim(), COMPACTION_TOOL_RESULT_MAX_CHARS)
                );
            }
            messages::ContentBlock::Image {
                mime_type,
                sha256,
                source,
                ..
            } => {
                let _ = writeln!(output, "image: {source} ({mime_type}, sha256:{sha256})");
            }
            _ => {}
        }
    }
    output
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n[truncated]")
    } else {
        truncated
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

fn parse_skill_invocation(input: &str) -> Option<(&str, Option<String>)> {
    if let Some(rest) = input.strip_prefix("/skill:") {
        let (name, args) = split_name_args(rest);
        return (!name.is_empty()).then_some((name, args));
    }
    if let Some(rest) = input.strip_prefix("/skill ") {
        let (name, args) = split_name_args(rest.trim());
        return (!name.is_empty()).then_some((name, args));
    }
    None
}

fn split_name_args(input: &str) -> (&str, Option<String>) {
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let args = parts
        .next()
        .map(str::trim)
        .filter(|args| !args.is_empty())
        .map(str::to_string);
    (name, args)
}

fn should_handle_as_command(input: &str, cwd: &Path) -> bool {
    let first = input.split_whitespace().next().unwrap_or("");
    if first.is_empty() || !first.starts_with('/') {
        return false;
    }
    if looks_like_image_path(first)
        && builtin_tools::path::resolve_to_cwd(first, cwd).is_ok_and(|path| path.is_file())
    {
        return false;
    }
    true
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

async fn handle_bang_command(
    input: &str,
    config: &Config,
    state: &mut AgentState,
) -> Result<CommandAction> {
    let (send_to_model, command) = if let Some(command) = input.strip_prefix("!!") {
        (false, command.trim())
    } else if let Some(command) = input.strip_prefix('!') {
        (true, command.trim())
    } else {
        unreachable!()
    };

    if command.is_empty() {
        anyhow::bail!("usage: !<command> or !!<command>");
    }

    eprintln!("[bash] {command}");
    let output = builtin_tools::bash::run(command, &state.cwd, Duration::from_secs(120)).await?;
    let rendered = render_bash_output(command, &output);

    if send_to_model {
        state.run_turn(rendered, config).await?;
        println!();
    } else {
        print!("{rendered}");
        if !rendered.ends_with('\n') {
            println!();
        }
    }

    Ok(CommandAction::Continue)
}

fn render_bash_output(command: &str, output: &builtin_tools::bash::BashOutput) -> String {
    format!(
        "Shell command executed: `{}`\nstatus: {:?}\ntimed_out: {}\nstdout:\n{}\nstderr:\n{}",
        command, output.status, output.timed_out, output.stdout, output.stderr
    )
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
            println!("  /version              show Ferrum version");
            println!("  /session              show session path/status/size");
            println!("  /sessions             list recent sessions for current directory");
            println!("  /sessions <ref>       open bracket number, id prefix, or path");
            println!("  /sessions pick        open numbered session picker");
            println!("  /sessions new         start a new session");
            println!("  /skills               list available skills");
            println!("  /skill <name> [args]  load a skill into context");
            println!("  /skill:<name> [args]  load a skill into context");
            println!("  /model [name]         show or set model");
            println!("  /models               list known models for current provider");
            println!("  /provider [name]      show or set provider");
            println!("  /providers            list configured providers");
            println!("  /mcp [on|off|status]  show or toggle MCP tools");
            println!(
                "  /thinking [level]     show or set thinking: off|minimal|low|medium|high|xhigh"
            );
            println!("  /image <path>         attach image to next message");
            println!("  /paste-image          attach image from clipboard");
            println!("  !<cmd>                run shell command and send output to model");
            println!("  !!<cmd>               run shell command and print output only");
            println!("  /compact              compact current in-memory conversation");
            Ok(CommandAction::Continue)
        }
        "/version" => {
            println!("ferrum {}", env!("CARGO_PKG_VERSION"));
            Ok(CommandAction::Continue)
        }
        "/session" => {
            println!("path: {}", state.session.path().display());
            let stats = state.stats();
            println!("messages: {}", stats.messages);
            println!("chars: {}", stats.chars);
            println!("estimated_tokens: {}", stats.estimated_tokens);
            println!("max_context_tokens: {}", config.max_context_tokens);
            println!("max_tool_rounds: {}", config.max_tool_rounds);
            println!("file_bytes: {}", stats.file_bytes);
            println!("pending_images: {}", state.pending_images.len());
            println!("skills: {}", state.skills.len());
            println!("mcp_enabled: {}", state.mcp_enabled);
            println!("mcp_connected: {}", state.mcp.is_some());
            println!("model: {}", config.model);
            println!("thinking: {:?}", config.thinking);
            println!("provider: {}", config.provider_name);
            Ok(CommandAction::Continue)
        }
        "/sessions" => {
            match parts.next() {
                None => state.list_sessions(config)?,
                Some("pick") => state.pick_session(config)?,
                Some("new") => state.new_session(config)?,
                Some("open") => {
                    let reference = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("usage: /sessions open <number|id|path>"))?;
                    state.open_session_reference(config, reference)?;
                }
                Some(reference) => state.open_session_reference(config, reference)?,
            }
            Ok(CommandAction::Continue)
        }
        "/skills" => {
            if state.skills.is_empty() {
                println!("no skills found");
            } else {
                for skill in &state.skills {
                    println!("{} - {}", skill.name, skill.description);
                    println!("  {}", skill.path.display());
                }
            }
            Ok(CommandAction::Continue)
        }
        "/skill" => {
            anyhow::bail!("usage: /skill:<name> [args]")
        }
        command if command.starts_with("/skill:") => {
            anyhow::bail!("unknown skill invocation: {command}")
        }
        "/model" => {
            if let Some(model) = parts.next() {
                config.model = model.to_string();
            }
            println!("model: {}", config.model);
            Ok(CommandAction::Continue)
        }
        "/models" => {
            anyhow::bail!("/models is async; this command should be handled before sync commands")
        }
        "/provider" => {
            if let Some(provider) = parts.next() {
                config.set_provider(provider)?;
            }
            println!("provider: {}", config.provider_name);
            println!("model: {}", config.model);
            Ok(CommandAction::Continue)
        }
        "/providers" => {
            if config.providers.is_empty() {
                println!("no configured providers in config.toml");
            } else {
                for (name, definition) in &config.providers {
                    let marker = if name == &config.provider_name {
                        "*"
                    } else {
                        " "
                    };
                    let default_model = definition
                        .default_model
                        .as_deref()
                        .map(|model| format!(" default_model={model}"))
                        .unwrap_or_default();
                    println!("{marker} {name} type={}{}", definition.kind, default_model);
                }
            }
            Ok(CommandAction::Continue)
        }
        "/mcp" => {
            match parts.next() {
                None | Some("status") | Some("list") => state.print_mcp_status(config),
                Some("on") => {
                    config.mcp_enabled = true;
                    state.set_mcp_enabled(true)?;
                }
                Some("off") => {
                    config.mcp_enabled = false;
                    state.set_mcp_enabled(false)?;
                }
                Some(other) => anyhow::bail!("usage: /mcp [on|off|status], got: {other}"),
            }
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
            anyhow::bail!("/compact is async; this command should be handled before sync commands")
        }
        _ => {
            println!("unknown command: {command}");
            Ok(CommandAction::Continue)
        }
    }
}
