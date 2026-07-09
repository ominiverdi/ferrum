pub mod messages;
pub mod tools;

use crate::{
    config::{ColorMode, Config, DiffMode, SafetyLevel, ToolSelection},
    context, mcp, providers, session, skills, tools as builtin_tools, ui_colors, usage,
};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal,
};
use rustyline::{
    Editor, Helper,
    completion::{Completer, Pair},
    error::ReadlineError,
    highlight::Highlighter,
    hint::Hinter,
    history::DefaultHistory,
    validate::Validator,
};
use similar::{ChangeTag, TextDiff};
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as FmtWrite,
    fs::{self, OpenOptions},
    io::{self, IsTerminal, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use ui_colors::{ColorPalette, ColorToken};

const COMPACTION_KEEP_RECENT_TOKENS: usize = 20_000;
const COMPACTION_TOOL_RESULT_MAX_CHARS: usize = 2_000;
const LOCAL_COMPACTION_SUMMARY_MAX_CHARS: usize = 16_000;
const TOOL_PREVIEW_MAX_CHARS: usize = 4_000;
const CONTEXT_ADVISORY_PERCENT: usize = 75;
const CONTEXT_WARNING_PERCENT: usize = 85;
const CONTEXT_CRITICAL_PERCENT: usize = 92;
const CONTEXT_AUTO_COMPACT_PERCENT: usize = 95;
const CONTEXT_RESERVE_TOKENS: usize = 16_384;
const HARD_TOOL_ROUND_LIMIT: usize = 256;
const REPEATED_TOOL_NUDGE_LIMIT: usize = 4;
const REPEATED_TOOL_FORCE_LIMIT: usize = 7;
const CONSECUTIVE_ERROR_NUDGE_LIMIT: usize = 5;
const CONSECUTIVE_ERROR_FORCE_LIMIT: usize = 8;

#[derive(Default)]
struct FerrumLineHelper {
    command_hints: HashMap<&'static str, &'static str>,
    skill_names: Vec<String>,
    model_names: Vec<String>,
    provider_names: Vec<String>,
    palette_names: Vec<String>,
}

impl Helper for FerrumLineHelper {}
impl Validator for FerrumLineHelper {}
impl Highlighter for FerrumLineHelper {}

impl Hinter for FerrumLineHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
        if pos != line.len() {
            return None;
        }
        let command = line.trim_start();
        if command == "/palette" {
            return Some(" <name>  (/palettes to list)".to_string());
        }
        if let Some(prefix) = command.strip_prefix("/palette ") {
            if prefix.split_whitespace().count() > 1 {
                return None;
            }
            if prefix.is_empty() {
                return Some(palette_list_hint(&self.palette_names));
            }
            return self
                .palette_names
                .iter()
                .find(|name| name.starts_with(prefix))
                .and_then(|name| name.strip_prefix(prefix))
                .filter(|rest| !rest.is_empty())
                .map(str::to_string);
        }
        if line.chars().last().is_some_and(char::is_whitespace) {
            return None;
        }
        self.command_hints
            .iter()
            .find_map(|(prefix, hint)| command.eq(*prefix).then(|| (*hint).to_string()))
    }
}

impl Completer for FerrumLineHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before = &line[..pos];
        let leading_spaces = before.len() - before.trim_start().len();
        let command_before = &before[leading_spaces..];
        if let Some(prefix) = command_before.strip_prefix("/image ") {
            let start = pos - prefix.len();
            return Ok((start, complete_path_candidates(prefix)));
        }
        if let Some(prefix) = command_before.strip_prefix("/skill:") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_owned_words(prefix, &self.skill_names)));
        }
        if let Some(prefix) = command_before.strip_prefix("/skill ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_owned_words(prefix, &self.skill_names)));
        }
        if let Some(prefix) = command_before.strip_prefix("/session ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, session_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/sessions ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, sessions_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/model ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_owned_words(prefix, &self.model_names)));
        }
        if let Some(prefix) = command_before.strip_prefix("/provider ") {
            let start = pos - prefix.len();
            return Ok((
                start,
                complete_from_owned_words(prefix, &self.provider_names),
            ));
        }
        if let Some(prefix) = command_before.strip_prefix("/thinking ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, thinking_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/safety ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, safety_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/diff ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, diff_mode_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/mcp ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, mcp_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/colors ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, color_words())));
        }
        if let Some(prefix) = command_before.strip_prefix("/palette ") {
            let start = pos - prefix.len();
            return Ok((
                start,
                complete_from_owned_words(prefix, &self.palette_names),
            ));
        }
        if let Some(prefix) = command_before.strip_prefix("/usage ") {
            let start = pos - prefix.len();
            return Ok((start, complete_from_words(prefix, usage_words())));
        }
        if command_before.starts_with('/') && !command_before.chars().any(char::is_whitespace) {
            let start = leading_spaces + command_before.rfind('/').unwrap_or(0);
            return Ok((
                start,
                complete_from_words(command_before, slash_command_words()),
            ));
        }
        Ok((pos, Vec::new()))
    }
}

impl FerrumLineHelper {
    fn new(skills: &[skills::Skill], config: &Config) -> Self {
        let mut command_hints = HashMap::new();
        command_hints.insert("/image", " <path>");
        command_hints.insert("/image-paste", "");
        command_hints.insert("/paste-image", "");
        command_hints.insert("/session", "");
        command_hints.insert("/sessions", " pick | del | new");
        command_hints.insert("/colors", " auto|on|off");
        command_hints.insert("/palette", " <name>  (/palettes to list)");
        command_hints.insert("/palettes", "");
        command_hints.insert("/model", " <name>");
        command_hints.insert("/provider", " <name>");
        command_hints.insert("/thinking", " off|minimal|low|medium|high|xhigh");
        command_hints.insert("/safety", " low|medium|high");
        command_hints.insert("/diff", " unified|compact|full|words|side_by_side");
        command_hints.insert("/mcp", " on|off|status|list");
        command_hints.insert("/usage", " day|week|month");
        let skill_names = skill_command_words(skills);
        let model_names = model_command_words(config);
        let provider_names = provider_command_words(config);
        let palette_names = palette_command_words(config);
        Self {
            command_hints,
            skill_names,
            model_names,
            provider_names,
            palette_names,
        }
    }
}

fn slash_command_words() -> &'static [&'static str] {
    &[
        "/help",
        "/quit",
        "/exit",
        "/version",
        "/session",
        "/sessions",
        "/title",
        "/skills",
        "/skill",
        "/model",
        "/models",
        "/provider",
        "/providers",
        "/thinking",
        "/safety",
        "/mcp",
        "/colors",
        "/palette",
        "/palettes",
        "/diff",
        "/image",
        "/image-paste",
        "/usage",
        "/paste-image",
        "/compact",
    ]
}

fn skill_command_words(skills: &[skills::Skill]) -> Vec<String> {
    skills.iter().map(|skill| skill.name.clone()).collect()
}

fn model_command_words(config: &Config) -> Vec<String> {
    let mut names = config.models.keys().cloned().collect::<Vec<_>>();
    if !names.iter().any(|name| name == &config.model) {
        names.push(config.model.clone());
    }
    names.sort();
    names.dedup();
    names
}

fn provider_command_words(config: &Config) -> Vec<String> {
    let mut names = config.providers.keys().cloned().collect::<Vec<_>>();
    if !names.iter().any(|name| name == &config.provider_name) {
        names.push(config.provider_name.clone());
    }
    names.sort();
    names.dedup();
    names
}

fn palette_list_hint(names: &[String]) -> String {
    if names.is_empty() {
        return " <name>".to_string();
    }
    let joined = names.iter().take(8).cloned().collect::<Vec<_>>().join("|");
    if names.len() > 8 {
        format!(" {joined}|...")
    } else {
        format!(" {joined}")
    }
}

fn palette_command_words(config: &Config) -> Vec<String> {
    list_palette_names(&config.config_dir).unwrap_or_default()
}

fn session_words() -> &'static [&'static str] {
    &[]
}

fn sessions_words() -> &'static [&'static str] {
    &["pick", "del", "new"]
}

fn color_words() -> &'static [&'static str] {
    &["auto", "on", "off"]
}

fn thinking_words() -> &'static [&'static str] {
    &["off", "minimal", "low", "medium", "high", "xhigh"]
}

fn safety_words() -> &'static [&'static str] {
    &["low", "medium", "high"]
}

fn diff_mode_words() -> &'static [&'static str] {
    &["unified", "compact", "full", "words", "side_by_side"]
}

fn mcp_words() -> &'static [&'static str] {
    &["on", "off", "status", "list"]
}

fn usage_words() -> &'static [&'static str] {
    &["day", "week", "month"]
}

fn list_palette_names(config_dir: &Path) -> Result<Vec<String>> {
    let palette_dir = config_dir.join("color-palettes");
    let Ok(entries) = fs::read_dir(&palette_dir) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", palette_dir.display()))?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn palette_file_path(config_dir: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim().strip_suffix(".toml").unwrap_or(name.trim());
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        anyhow::bail!("invalid palette name: {name}");
    }
    Ok(config_dir
        .join("color-palettes")
        .join(format!("{name}.toml")))
}

fn print_palette_list(config_dir: &Path) -> Result<()> {
    let names = list_palette_names(config_dir)?;
    if names.is_empty() {
        println!(
            "no palettes found in {}",
            config_dir.join("color-palettes").display()
        );
    } else {
        for name in names {
            println!("{name}");
        }
    }
    Ok(())
}

fn current_palette_name(config_dir: &Path, colors: &ColorPalette) -> Result<String> {
    let colors_path = config_dir.join("colors.toml");
    if !colors_path.exists() {
        return Ok("default".to_string());
    }
    for name in list_palette_names(config_dir)? {
        let path = palette_file_path(config_dir, &name)?;
        let Ok(palette) = ColorPalette::load_palette_file(&path) else {
            continue;
        };
        if &palette == colors {
            return Ok(name);
        }
    }
    Ok("custom".to_string())
}

fn apply_palette(name: &str, config: &mut Config, state: &mut AgentState) -> Result<()> {
    let path = palette_file_path(&config.config_dir, name)?;
    if !path.exists() {
        anyhow::bail!("unknown palette: {name}. Use /palettes to list available palettes");
    }
    let palette = ColorPalette::load_palette_file(&path)?;
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    fs::create_dir_all(&config.config_dir)
        .with_context(|| format!("failed to create {}", config.config_dir.display()))?;
    let colors_path = config.config_dir.join("colors.toml");
    fs::write(&colors_path, text)
        .with_context(|| format!("failed to write {}", colors_path.display()))?;
    config.colors = palette.clone();
    state.colors = palette;
    println!(
        "palette: {}",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(name)
    );
    Ok(())
}

fn complete_from_words(prefix: &str, words: &[&str]) -> Vec<Pair> {
    words
        .iter()
        .filter(|word| word.starts_with(prefix))
        .map(|word| Pair {
            display: (*word).to_string(),
            replacement: (*word).to_string(),
        })
        .collect()
}

fn complete_from_owned_words(prefix: &str, words: &[String]) -> Vec<Pair> {
    words
        .iter()
        .filter(|word| word.starts_with(prefix))
        .map(|word| Pair {
            display: word.clone(),
            replacement: word.clone(),
        })
        .collect()
}

fn complete_path_candidates(prefix: &str) -> Vec<Pair> {
    let (typed_dir, needle) = match prefix.rsplit_once('/') {
        Some((dir, needle)) => (dir, needle),
        None => (".", prefix),
    };
    let dir = expand_tilde_path(typed_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(needle) {
            continue;
        }
        let mut replacement = if prefix.contains('/') {
            match typed_dir {
                "." => name.to_string(),
                "/" => format!("/{name}"),
                dir_text => format!("{dir_text}/{name}"),
            }
        } else {
            name.to_string()
        };
        if path.is_dir() {
            replacement.push('/');
        }
        matches.push(Pair {
            display: replacement.clone(),
            replacement,
        });
    }
    matches.sort_by(|a, b| a.display.cmp(&b.display));
    matches
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

pub async fn run_print(
    prompt: String,
    images: Vec<String>,
    session_ref: Option<&str>,
    title: Option<&str>,
    config: &Config,
) -> Result<()> {
    let mut effective_config = config.clone();
    let mut state = if let Some(reference) = session_ref {
        AgentState::resume_or_create_ref(&mut effective_config, reference)?
    } else {
        AgentState::new(&effective_config)?
    };
    if let Some(title) = title {
        state.set_title(title)?;
    }
    state.attach_images(images)?;
    let (prompt, pasted_images) = extract_pasted_images(&prompt, &state.cwd);
    state.attach_images(pasted_images)?;
    state.run_turn(prompt, &effective_config, false).await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_interactive(
    config: &mut Config,
    resume: Option<Option<String>>,
    continue_latest: bool,
    session_ref: Option<String>,
    title: Option<&str>,
    thinking_overridden: bool,
    safety_overridden: bool,
    tools_overridden: bool,
    provider_overridden: bool,
    model_overridden: bool,
) -> Result<()> {
    let show_resume_tail = session_ref.is_some() || resume.is_some() || continue_latest;
    let mut state = match (session_ref, resume, continue_latest) {
        (Some(reference), _, _) => AgentState::resume_ref(
            config,
            Some(&reference),
            !thinking_overridden,
            !safety_overridden,
            !tools_overridden,
            !provider_overridden,
            !model_overridden,
        )?,
        (None, Some(Some(reference)), _) => AgentState::resume_ref(
            config,
            Some(&reference),
            !thinking_overridden,
            !safety_overridden,
            !tools_overridden,
            !provider_overridden,
            !model_overridden,
        )?,
        (None, Some(None), _) | (None, None, true) => AgentState::resume_ref(
            config,
            None,
            !thinking_overridden,
            !safety_overridden,
            !tools_overridden,
            !provider_overridden,
            !model_overridden,
        )?,
        (None, None, false) => AgentState::new(config)?,
    };
    if let Some(title) = title {
        state.set_title(title)?;
    }
    println!("Ferrum interactive. /help for commands.");
    print_current_session_header(&state)?;
    if show_resume_tail {
        print_recent_conversation_lines(&state.messages, 40, state.color_mode, &state.colors);
    }

    let mut rl = Editor::<FerrumLineHelper, DefaultHistory>::new()?;
    rl.set_helper(Some(FerrumLineHelper::new(&state.skills, config)));
    let history = config.history_path();
    let _ = prepare_history_file(&history);
    let _ = rl.load_history(&history);

    let mut last_ctrl_c: Option<Instant> = None;
    loop {
        let prompt = state
            .colors
            .paint(ColorToken::Prompt, state.color_mode, "ferrum> ");
        match rl.readline(&prompt) {
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
                let history_input = sanitize_history_input(input);
                let _ = rl.add_history_entry(history_input.as_str());
                if input.starts_with('!') {
                    match handle_bang_command(input, config, &mut state).await {
                        Ok(CommandAction::Continue) => continue,
                        Ok(CommandAction::Quit) => {
                            state.remove_empty_session()?;
                            save_history_private(&mut rl, &history);
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
                        Ok(prompt) => match state.run_turn(prompt, config, true).await {
                            Ok(()) => render_prompt_separator(state.color_mode, &state.colors),
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
                    let mut abort = ActiveTurnAbort::start(true);
                    let result = state
                        .compact(config, instructions, false, Some(abort.token()))
                        .await;
                    abort.stop();
                    match result {
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
                        Err(error) if error.to_string() == "aborted" => println!("aborted"),
                        Err(error) => eprintln!("Error: {error}"),
                    }
                    continue;
                }
                if should_handle_as_command(input, &state.cwd) {
                    match handle_command(input, config, &mut state) {
                        Ok(CommandAction::Continue) => continue,
                        Ok(CommandAction::Quit) => {
                            state.remove_empty_session()?;
                            save_history_private(&mut rl, &history);
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
                match state.run_turn(prompt, config, true).await {
                    Ok(()) => render_prompt_separator(state.color_mode, &state.colors),
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
                    state.remove_empty_session()?;
                    save_history_private(&mut rl, &history);
                    return Ok(());
                }
                last_ctrl_c = Some(now);
                println!("^C (press Ctrl+C again to quit)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!();
                state.remove_empty_session()?;
                save_history_private(&mut rl, &history);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn restore_session_preferences(
    config: &mut Config,
    path: &Path,
    restore_thinking: bool,
    restore_safety: bool,
    restore_tools: bool,
    restore_provider: bool,
    restore_model: bool,
) -> Result<Option<Vec<String>>> {
    let Some(info) = session::jsonl::session_info(path)? else {
        return Ok(None);
    };
    if restore_provider && let Some(provider) = info.provider.as_deref() {
        config.set_provider(provider)?;
    }
    if restore_model && let Some(model) = info.model.as_deref() {
        config.set_model(model)?;
    }
    if restore_thinking && let Some(thinking) = info.thinking.as_deref() {
        config.thinking = crate::config::ThinkingLevel::parse(thinking)?;
    }
    if let Some(diff_mode) = info.diff_mode.as_deref() {
        config.diff_mode = DiffMode::parse(diff_mode)?;
    }
    if restore_safety && let Some(safety) = info.safety.as_deref() {
        config.safety = SafetyLevel::parse(safety)?;
    }
    if let Some(color_mode) = info.color_mode.as_deref() {
        config.color_mode = ColorMode::parse(color_mode)?;
    }
    Ok(restore_tools.then_some(info.tools).flatten())
}

fn runtime_context(config: &Config, cwd: &Path) -> Result<String> {
    let system_prompt_path = config.config_dir.join("system.md");
    let template = if system_prompt_path.exists() {
        fs::read_to_string(&system_prompt_path)
            .with_context(|| format!("failed to read {}", system_prompt_path.display()))?
    } else {
        default_system_prompt_template().to_string()
    };
    Ok(render_system_prompt_template(&template, config, cwd))
}

fn default_system_prompt_template() -> &'static str {
    "You are running inside Ferrum, a Rust-native Linux coding agent.\n\nRuntime metadata:\n- ferrum_version: {{ferrum_version}}\n- provider: {{provider}}\n- model: {{model}}\n- provider_model: {{provider_model}}\n- thinking: {{thinking}}\n- cwd: {{cwd}}\n- config_dir: {{config_dir}}\n- max_context_tokens: {{max_context_tokens}}\n- max_tool_rounds: {{max_tool_rounds}}\n- mcp_enabled: {{mcp_enabled}}\n- diff_mode: {{diff_mode}}\n- safety: {{safety}}\n\nAgent behavior:\n- Be proactive. If the user asks you to investigate local state, use tools before asking for information that Ferrum can inspect.\n- Do not claim you searched something unless a tool result supports it.\n- Prefer targeted evidence over broad noisy scans. Start narrow, then widen deliberately.\n- For Linux desktop/service issues, check likely systemd user units, service files, logs, running processes, executable paths, environment/session type, and relevant config.\n- When using tools, read important files directly and cite exact paths, commands, and error messages.\n- After several tool calls, synthesize what is known, what is still unknown, and the next concrete action. Do not loop indefinitely.\n- If the adaptive loop guard stops tool use, summarize findings from available evidence instead of continuing to search.\n\nTool usage guidance:\n- Use read for known files.\n- Batch independent tool calls in the same turn when possible, especially file inspection commands such as ls, read, grep, and find.\n- Prefer native ls/find/grep for filesystem exploration when they fit. They are safer and avoid noisy dependency/build directories.\n- Avoid broad bash find/grep over \".\" unless needed. If using shell find/grep, prune .git, target, node_modules, and other dependency/build directories.\n- Use bash for shell commands, systemctl, journalctl, process inspection, package checks, and focused pipelines.\n- Keep bash commands focused and safe. Avoid destructive commands unless the user explicitly asked for them.\n- For long-running or background scripts, use nohup with redirected logs and verify separately.\n\nInteractive commands available to the user:\n- /help\n- /version\n- /session\n- /title [text]\n- /sessions\n- /sessions pick\n- /sessions del\n- /sessions new\n- /model [name]\n- /models\n- /usage [day|week|month]\n- /provider [name]\n- /providers\n- /mcp [on|off|status|list]\n- /colors [auto|on|off]\n- /palette [name]\n- /palettes\n- /thinking [off|minimal|low|medium|high|xhigh]\n- /safety [low|medium|high]\n- /diff [unified|compact|full|words|side_by_side]\n- /skills\n- /skill <name> [args]\n- /skill:<name> [args]\n- /image <path>\n- /image-paste\n- /paste-image\n- /compact\n- /quit\n- /exit\n\nShell shortcuts available to the user:\n- !<cmd>: run a shell command and send output to the model\n- !!<cmd>: run a shell command and show output only to the user\n\nThese slash commands and shell shortcuts are handled by Ferrum before user messages are sent to you. You cannot execute them by printing them; tell the user which command to run when needed."
}

fn render_system_prompt_template(template: &str, config: &Config, cwd: &Path) -> String {
    let replacements = [
        ("{{ferrum_version}}", env!("CARGO_PKG_VERSION").to_string()),
        ("{{provider}}", config.provider_name.clone()),
        ("{{model}}", config.model.clone()),
        ("{{provider_model}}", config.provider_model.clone()),
        ("{{thinking}}", config.thinking.as_str().to_string()),
        ("{{cwd}}", cwd.display().to_string()),
        ("{{config_dir}}", config.config_dir.display().to_string()),
        (
            "{{max_context_tokens}}",
            config.max_context_tokens.to_string(),
        ),
        ("{{max_tool_rounds}}", config.max_tool_rounds.to_string()),
        ("{{mcp_enabled}}", config.mcp_enabled.to_string()),
        ("{{diff_mode}}", config.diff_mode.as_str().to_string()),
        ("{{safety}}", config.safety.as_str().to_string()),
    ];
    let mut rendered = template.to_string();
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, &value);
    }
    rendered
}

#[derive(Debug)]
struct ExecutedToolUse {
    id: String,
    name: String,
    input: serde_json::Value,
    content: String,
    is_error: bool,
    aborted: bool,
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

static USAGE_WARNING_PRINTED: AtomicBool = AtomicBool::new(false);

fn append_usage_record_with_warning(data_dir: &Path, record: &usage::UsageRecord) {
    if let Err(error) = usage::append_usage_record(data_dir, record)
        && !USAGE_WARNING_PRINTED.swap(true, Ordering::Relaxed)
    {
        eprintln!("[usage] failed to persist usage: {error}");
    }
}

fn usage_for_response(
    provider_usage: Option<messages::TokenUsage>,
    request_messages: &[messages::Message],
    tools: &[tools::ToolDefinition],
    response: &messages::Message,
) -> messages::TokenUsage {
    provider_usage
        .unwrap_or_else(|| estimated_usage_for_response(request_messages, tools, response))
}

fn estimated_usage_for_response(
    request_messages: &[messages::Message],
    tools: &[tools::ToolDefinition],
    response: &messages::Message,
) -> messages::TokenUsage {
    let input_tokens = estimated_request_tokens(request_messages, tools) as u64;
    let output_tokens = estimated_tokens_for_message(response) as u64;
    messages::TokenUsage {
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        total_tokens: Some(input_tokens.saturating_add(output_tokens)),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        source: "estimated".to_string(),
    }
}

fn estimated_request_tokens(
    messages: &[messages::Message],
    tools: &[tools::ToolDefinition],
) -> usize {
    let message_tokens = estimated_tokens_for_messages(messages);
    let tool_tokens = serde_json::to_vec(tools)
        .map(|bytes| bytes.len().div_ceil(4))
        .unwrap_or(0);
    message_tokens.saturating_add(tool_tokens)
}

struct ActiveTurnAbort {
    aborted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ActiveTurnAbort {
    fn start(enabled: bool) -> Self {
        let aborted = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        if !enabled || !io::stdin().is_terminal() {
            return Self {
                aborted,
                stop,
                handle: None,
            };
        }
        let watcher_aborted = Arc::clone(&aborted);
        let watcher_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let _ = terminal::enable_raw_mode();
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                let _ = event::read();
            }
            while !watcher_stop.load(Ordering::Relaxed) {
                if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(key))
                            if key.code == KeyCode::Esc
                                || key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            watcher_aborted.store(true, Ordering::Relaxed);
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = terminal::disable_raw_mode();
        });
        Self {
            aborted,
            stop,
            handle: Some(handle),
        }
    }

    fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.aborted)
    }

    fn is_cancelled(&self) -> bool {
        self.aborted.load(Ordering::Relaxed)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for ActiveTurnAbort {
    fn drop(&mut self) {
        self.stop();
    }
}

struct LiveRenderState {
    color_mode: ColorMode,
    colors: ColorPalette,
    thinking_started: bool,
    text_started: bool,
    text_ended_with_newline: bool,
}

impl LiveRenderState {
    fn new(color_mode: ColorMode, colors: ColorPalette) -> Self {
        Self {
            color_mode,
            colors,
            thinking_started: false,
            text_started: false,
            text_ended_with_newline: false,
        }
    }

    fn render_event(&mut self, event: providers::StreamEvent) -> Result<()> {
        match event {
            providers::StreamEvent::ThinkingDelta(delta) => {
                if !self.thinking_started {
                    self.thinking_started = true;
                    print_raw_mode_text_styled(
                        "thinking:\n",
                        ColorToken::Thinking,
                        self.color_mode,
                        &self.colors,
                    );
                }
                print_raw_mode_text_styled(
                    &delta,
                    ColorToken::Thinking,
                    self.color_mode,
                    &self.colors,
                );
            }
            providers::StreamEvent::TextDelta(delta) => {
                if !self.text_started {
                    self.text_started = true;
                    if self.thinking_started {
                        print_raw_mode_text_styled(
                            "\r\n------\r\n",
                            ColorToken::Hr,
                            self.color_mode,
                            &self.colors,
                        );
                    }
                }
                self.text_ended_with_newline = delta.ends_with('\n');
                print_raw_mode_text_styled(
                    &delta,
                    ColorToken::Assistant,
                    self.color_mode,
                    &self.colors,
                );
            }
        }
        io::stdout().flush()?;
        Ok(())
    }

    fn finish(&self) -> Result<()> {
        if self.text_started && !self.text_ended_with_newline {
            println!();
        }
        io::stdout().flush()?;
        Ok(())
    }

    fn started(&self) -> bool {
        self.thinking_started || self.text_started
    }
}

fn print_raw_mode_text_styled(
    text: &str,
    token: ColorToken,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    let text = text.replace('\n', "\r\n");
    let (prefix, suffix) = colors.prefix_suffix(token, color_mode);
    print!("{prefix}{text}{suffix}");
}

fn render_hr(color_mode: ColorMode, colors: &ColorPalette) {
    println!("{}", colors.paint(ColorToken::Hr, color_mode, "------"));
}

fn render_turn_separator(color_mode: ColorMode, colors: &ColorPalette) {
    println!();
    render_hr(color_mode, colors);
}

fn render_status_notice(message: &str, color_mode: ColorMode, colors: &ColorPalette) {
    println!();
    render_hr(color_mode, colors);
    println!("{}", colors.paint(ColorToken::Status, color_mode, message));
    render_hr(color_mode, colors);
}

fn render_prompt_separator(color_mode: ColorMode, colors: &ColorPalette) {
    println!();
    render_hr(color_mode, colors);
}

fn render_assistant_response(
    response: &messages::Message,
    interactive: bool,
    color_mode: ColorMode,
    colors: &ColorPalette,
) -> Result<()> {
    let output_color_mode = if interactive {
        color_mode
    } else {
        ColorMode::Off
    };
    let summary = response.thinking_text();
    if interactive && !summary.trim().is_empty() {
        println!(
            "{}",
            colors.paint(ColorToken::Thinking, output_color_mode, "thinking:")
        );
        println!(
            "{}",
            colors.paint(ColorToken::Thinking, output_color_mode, summary.trim())
        );
        println!();
        render_hr(output_color_mode, colors);
    }
    let text = response.display_text();
    print!(
        "{}",
        colors.paint(ColorToken::Assistant, output_color_mode, &text)
    );
    if !text.ends_with('\n') {
        println!();
    }
    io::stdout().flush()?;
    Ok(())
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

fn history_tool_definitions() -> Vec<tools::ToolDefinition> {
    vec![
        tools::ToolDefinition {
            name: "history_search".to_string(),
            description: "Search the current session history, including messages archived before compaction. Use when prior details from this conversation may matter. Returns matching snippets with JSONL line numbers for follow-up history_read calls.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text or regex to search for" },
                    "literal": { "type": "boolean", "description": "Treat query as literal text. Default true." },
                    "ignore_case": { "type": "boolean", "description": "Case-insensitive search. Default true." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum matches to return. Default 10." }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        tools::ToolDefinition {
            name: "history_read".to_string(),
            description: "Read rendered entries from the current session history by JSONL line number. Use line numbers returned by history_search to inspect surrounding context.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "offset": { "type": "integer", "minimum": 1, "description": "1-based JSONL line number to start reading" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum JSONL lines to read. Default 20." }
                },
                "required": ["offset"],
                "additionalProperties": false
            }),
        },
    ]
}

fn required_history_str<'a>(input: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required string field: {key}"))
}

fn active_mcp_servers(config: &Config) -> Vec<crate::config::McpServerConfig> {
    config
        .mcp_servers
        .iter()
        .filter(|server| {
            server.enabled
                && config
                    .mcp_server_allow
                    .as_ref()
                    .is_none_or(|allow| allow.iter().any(|name| name == &server.name))
        })
        .cloned()
        .collect()
}

fn resolve_available_tools(
    tools: Vec<tools::ToolDefinition>,
    config: &Config,
) -> Result<Vec<tools::ToolDefinition>> {
    let known = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    if known.contains("none") {
        anyhow::bail!("tool name 'none' is reserved");
    }

    validate_known_tools(config.tools_allow.as_deref(), &known, "config tools.allow")?;
    validate_known_tools(
        Some(config.tools_deny.as_slice()),
        &known,
        "config tools.deny",
    )?;

    let mut requested = match &config.tool_selection {
        None => None,
        Some(ToolSelection::None) => Some(HashSet::new()),
        Some(ToolSelection::List(names)) => {
            validate_known_tools(Some(names.as_slice()), &known, "--tools")?;
            Some(names.iter().map(String::as_str).collect::<HashSet<_>>())
        }
    };

    if let Some(allow) = &config.tools_allow {
        let allow = allow.iter().map(String::as_str).collect::<HashSet<_>>();
        if let Some(ToolSelection::List(names)) = &config.tool_selection {
            for name in names {
                if !allow.contains(name.as_str()) {
                    anyhow::bail!("tool '{name}' requested by --tools but not allowed by config");
                }
            }
        }
        match &mut requested {
            Some(requested) => requested.retain(|name| allow.contains(name)),
            None => requested = Some(allow),
        }
    }

    let deny = config
        .tools_deny
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(ToolSelection::List(names)) = &config.tool_selection {
        for name in names {
            if deny.contains(name.as_str()) {
                anyhow::bail!("tool '{name}' requested by --tools but denied by config");
            }
        }
    }

    let mut available = tools
        .into_iter()
        .filter(|tool| {
            let name = tool.name.as_str();
            requested.as_ref().is_none_or(|set| set.contains(name)) && !deny.contains(name)
        })
        .collect::<Vec<_>>();
    let bash_available = available.iter().any(|tool| tool.name == "bash");
    if !bash_available {
        available.retain(|tool| tool.name != "wait");
    }
    Ok(available)
}

fn validate_known_tools(
    names: Option<&[String]>,
    known: &HashSet<&str>,
    source: &str,
) -> Result<()> {
    let Some(names) = names else {
        return Ok(());
    };
    for name in names {
        if !known.contains(name.as_str()) {
            anyhow::bail!("unknown tool '{name}' in {source}");
        }
    }
    Ok(())
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
    color_mode: ColorMode,
    colors: ColorPalette,
    diff_mode: DiffMode,
    safety: SafetyLevel,
    pending_images: Vec<messages::ContentBlock>,
    last_session_list: Vec<session::jsonl::SessionInfo>,
    active_tool_names: HashSet<String>,
    saved_tool_names: Option<Vec<String>>,
    last_context_warning_bucket: Option<usize>,
}

impl AgentState {
    fn new(config: &Config) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut messages = Vec::new();
        messages.push(messages::Message::text(
            messages::Role::System,
            runtime_context(config, &cwd)?,
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
            session: session::JsonlSession::create_with_color_mode(
                config.sessions_dir(),
                Some(config.provider_name.clone()),
                Some(config.model.clone()),
                Some(config.thinking.as_str().to_string()),
                Some(config.color_mode.as_str().to_string()),
                Some(config.diff_mode.as_str().to_string()),
                Some(config.safety.as_str().to_string()),
                None,
            )?,
            messages,
            skills,
            cwd,
            mcp: None,
            mcp_enabled: config.mcp_enabled,
            color_mode: config.color_mode,
            colors: config.colors.clone(),
            diff_mode: config.diff_mode,
            safety: config.safety,
            pending_images: Vec::new(),
            last_session_list: Vec::new(),
            active_tool_names: HashSet::new(),
            saved_tool_names: None,
            last_context_warning_bucket: None,
        })
    }

    fn resume_ref(
        config: &mut Config,
        reference: Option<&str>,
        restore_thinking: bool,
        restore_safety: bool,
        restore_tools: bool,
        restore_provider: bool,
        restore_model: bool,
    ) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let path = match reference {
            Some(reference) => {
                session::jsonl::resolve_session_ref(&config.sessions_dir(), &cwd, reference)?
            }
            None => match session::jsonl::latest_session_for_cwd(&config.sessions_dir(), &cwd)? {
                Some(path) => path,
                None => {
                    eprintln!(
                        "no sessions found for {}; starting a new session",
                        cwd.display()
                    );
                    return Self::new(config);
                }
            },
        };
        let _restored_tools = restore_session_preferences(
            config,
            &path,
            restore_thinking,
            restore_safety,
            restore_tools,
            restore_provider,
            restore_model,
        )?;
        Self::open_session(config, path)
    }

    fn resume_or_create_ref(config: &mut Config, reference: &str) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let resolution = session::jsonl::resolve_or_create_session_ref(
            &config.sessions_dir(),
            &cwd,
            reference,
            Some(config.provider_name.clone()),
            Some(config.model.clone()),
            Some(config.thinking.as_str().to_string()),
            Some(config.color_mode.as_str().to_string()),
            Some(config.diff_mode.as_str().to_string()),
            Some(config.safety.as_str().to_string()),
            None,
        )?;
        match resolution {
            session::jsonl::SessionRefResolution::Existing(path) => {
                let _restored_tools =
                    restore_session_preferences(config, &path, true, true, true, true, true)?;
                let state = Self::open_session(config, path.clone())?;
                println!(
                    "resumed {} ({} messages)",
                    path.display(),
                    state.message_count()
                );
                print_session_preview(&state.messages, 2);
                Ok(state)
            }
            session::jsonl::SessionRefResolution::Created(path) => {
                let state = Self::open_session_with_preview(config, path.clone(), false)?;
                println!("started named session {}", path.display());
                Ok(state)
            }
        }
    }

    fn open_session(config: &Config, path: PathBuf) -> Result<Self> {
        Self::open_session_with_preview(config, path, false)
    }

    fn open_session_with_preview(
        config: &Config,
        path: PathBuf,
        show_preview: bool,
    ) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let saved_tool_names = session::jsonl::session_info(&path)?.and_then(|info| info.tools);
        let mut messages = session::jsonl::load_messages(&path)?;
        if show_preview {
            print_session_preview(&messages, 2);
        }
        messages.push(messages::Message::text(
            messages::Role::System,
            runtime_context(config, &cwd)?,
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
            color_mode: config.color_mode,
            colors: config.colors.clone(),
            diff_mode: config.diff_mode,
            safety: config.safety,
            pending_images: Vec::new(),
            last_session_list: Vec::new(),
            active_tool_names: HashSet::new(),
            saved_tool_names,
            last_context_warning_bucket: None,
        })
    }

    async fn run_turn(&mut self, prompt: String, config: &Config, interactive: bool) -> Result<()> {
        let stats = self.stats();
        if should_auto_compact(stats.estimated_tokens, config.max_context_tokens) {
            let percent = context_usage_percent(stats.estimated_tokens, config.max_context_tokens);
            eprintln!(
                "[session] context {percent}% used ({}/{} estimated tokens); compacting before limit",
                stats.estimated_tokens, config.max_context_tokens
            );
            let outcome = self.compact(config, None, true, None).await?;
            self.last_context_warning_bucket = None;
            match outcome {
                CompactionOutcome::Compacted {
                    before_tokens,
                    after_tokens,
                } => {
                    eprintln!(
                        "[session] compacted context: {before_tokens} -> {after_tokens} estimated tokens"
                    );
                    if should_auto_compact(after_tokens, config.max_context_tokens) {
                        let percent =
                            context_usage_percent(after_tokens, config.max_context_tokens);
                        eprintln!(
                            "[session] context remains above budget after compaction ({percent}% used, {after_tokens}/{} estimated tokens); continuing, but provider context errors are possible",
                            config.max_context_tokens
                        );
                    }
                }
                CompactionOutcome::Skipped {
                    reason,
                    before_tokens,
                    ..
                } => {
                    eprintln!("[session] compaction skipped: {reason}");
                    if should_auto_compact(before_tokens, config.max_context_tokens) {
                        let percent =
                            context_usage_percent(before_tokens, config.max_context_tokens);
                        eprintln!(
                            "[session] context remains above budget without compaction ({percent}% used, {before_tokens}/{} estimated tokens); continuing, but provider context errors are possible",
                            config.max_context_tokens
                        );
                    }
                }
            }
        } else {
            self.maybe_warn_context_pressure(
                stats.estimated_tokens,
                config.max_context_tokens,
                interactive,
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

        if interactive {
            render_turn_separator(self.color_mode, &self.colors);
            io::stdout().flush()?;
        }

        let provider = providers::from_config(&config.provider);
        let mut tools = builtin_tools::definitions();
        tools.extend(history_tool_definitions());
        if self.mcp_enabled {
            self.ensure_mcp(config).await?;
            if let Some(mcp) = &self.mcp {
                tools.extend_from_slice(mcp.definitions());
            }
        }
        tools = resolve_available_tools(tools, config)?;
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        self.active_tool_names = tool_names.iter().cloned().collect();
        if self.saved_tool_names.as_ref() != Some(&tool_names) {
            self.session.append_tools(&tool_names)?;
            self.saved_tool_names = Some(tool_names);
        }

        let metrics_enabled = metrics_enabled();
        let mut model_request_index = 0usize;
        let mut loop_guard = LoopGuard::new(config.max_tool_rounds);
        let mut overflow_recovery_attempted = false;
        let force_final_reason = loop {
            model_request_index += 1;
            if interactive && model_request_index > 1 {
                render_turn_separator(self.color_mode, &self.colors);
                io::stdout().flush()?;
            }
            let mut abort = ActiveTurnAbort::start(interactive);
            if metrics_enabled {
                emit_model_metrics_start(model_request_index, &self.messages, &tools);
            }
            let started = Instant::now();
            let mut live_render = LiveRenderState::new(self.color_mode, self.colors.clone());
            let mut on_event = |event| {
                let _ = live_render.render_event(event);
            };
            let response_result = if interactive {
                provider
                    .complete_streaming(
                        &config.provider_model,
                        &self.messages,
                        &tools,
                        config.thinking,
                        &mut on_event,
                        Some(abort.token()),
                    )
                    .await
            } else {
                provider
                    .complete(
                        &config.provider_model,
                        &self.messages,
                        &tools,
                        config.thinking,
                    )
                    .await
            };
            abort.stop();
            let response = match response_result {
                Ok(response) => response,
                Err(error) if error.to_string() == "aborted" => {
                    println!("aborted");
                    return Ok(());
                }
                Err(error) if is_context_overflow_error(&error) && !overflow_recovery_attempted => {
                    overflow_recovery_attempted = true;
                    eprintln!(
                        "[session] provider reported context overflow; compacting and retrying once"
                    );
                    let outcome = self.compact(config, None, true, None).await?;
                    self.last_context_warning_bucket = None;
                    match outcome {
                        CompactionOutcome::Compacted {
                            before_tokens,
                            after_tokens,
                        } => eprintln!(
                            "[session] compacted context after overflow: {before_tokens} -> {after_tokens} estimated tokens"
                        ),
                        CompactionOutcome::Skipped { reason, .. } => {
                            eprintln!("[session] overflow recovery compaction skipped: {reason}")
                        }
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            let provider_usage = response.usage;
            let mut response = response.message;
            let token_usage = usage_for_response(
                provider_usage.or_else(|| response.usage.clone()),
                &self.messages,
                &tools,
                &response,
            );
            if response.usage.is_none() {
                response.usage = Some(token_usage.clone());
            }
            append_usage_record_with_warning(
                &config.data_dir,
                &usage::UsageRecord {
                    timestamp_unix: usage::now_unix(),
                    provider: config.provider_name.clone(),
                    model: config.provider_model.clone(),
                    input_tokens: token_usage.input_tokens,
                    output_tokens: token_usage.output_tokens,
                    total_tokens: token_usage.total_tokens,
                    cache_read_tokens: token_usage.cache_read_tokens,
                    cache_write_tokens: token_usage.cache_write_tokens,
                    source: token_usage.source.clone(),
                },
            );
            if metrics_enabled {
                emit_model_metrics_end(model_request_index, started.elapsed(), &response);
            }
            if interactive {
                live_render.finish()?;
            }
            if !interactive || !live_render.started() {
                render_assistant_response(&response, interactive, self.color_mode, &self.colors)?;
            }
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
                self.maybe_warn_context_pressure(
                    self.stats().estimated_tokens,
                    config.max_context_tokens,
                    interactive,
                );
                return Ok(());
            }
            if abort.is_cancelled() {
                println!("aborted");
                return Ok(());
            }

            let executed_tools = self.execute_tool_batch(tool_uses, interactive).await;
            let aborted = executed_tools.iter().any(|tool| tool.aborted);
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
                    usage: None,
                };
                self.session.append_message(&result)?;
                self.messages.push(result);
            }
            if aborted {
                println!("aborted");
                return Ok(());
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
        let mut final_overflow_recovery_attempted = overflow_recovery_attempted;
        let final_response = loop {
            model_request_index += 1;
            if metrics_enabled {
                emit_model_metrics_start(model_request_index, &final_messages, &[]);
            }
            let started = Instant::now();
            let mut abort = ActiveTurnAbort::start(interactive);
            let mut live_render = LiveRenderState::new(self.color_mode, self.colors.clone());
            let mut on_event = |event| {
                let _ = live_render.render_event(event);
            };
            let response_result = if interactive {
                provider
                    .complete_streaming(
                        &config.provider_model,
                        &final_messages,
                        &[],
                        config.thinking,
                        &mut on_event,
                        Some(abort.token()),
                    )
                    .await
            } else {
                provider
                    .complete(
                        &config.provider_model,
                        &final_messages,
                        &[],
                        config.thinking,
                    )
                    .await
            };
            abort.stop();
            match response_result {
                Ok(response) => {
                    if metrics_enabled {
                        emit_model_metrics_end(
                            model_request_index,
                            started.elapsed(),
                            &response.message,
                        );
                    }
                    if interactive {
                        live_render.finish()?;
                    }
                    break (response, live_render.started());
                }
                Err(error) if error.to_string() == "aborted" => {
                    println!("aborted");
                    return Ok(());
                }
                Err(error)
                    if is_context_overflow_error(&error) && !final_overflow_recovery_attempted =>
                {
                    final_overflow_recovery_attempted = true;
                    eprintln!(
                        "[session] provider reported context overflow during final synthesis; compacting and retrying once"
                    );
                    let outcome = self.compact(config, None, true, None).await?;
                    self.last_context_warning_bucket = None;
                    match outcome {
                        CompactionOutcome::Compacted {
                            before_tokens,
                            after_tokens,
                        } => eprintln!(
                            "[session] compacted context before final synthesis retry: {before_tokens} -> {after_tokens} estimated tokens"
                        ),
                        CompactionOutcome::Skipped { reason, .. } => eprintln!(
                            "[session] final synthesis overflow recovery compaction skipped: {reason}"
                        ),
                    }
                    final_messages = self.messages.clone();
                    final_messages.push(messages::Message::text(
                        messages::Role::System,
                        format!(
                            "Adaptive loop guard stopped tool use: {force_final_reason}. Do not call tools. Summarize the findings from the available tool results, identify likely conclusions, and propose the next concrete step."
                        ),
                    ));
                }
                Err(error) => return Err(error),
            }
        };
        let (final_response, final_was_rendered_live) = final_response;
        let provider_usage = final_response.usage;
        let mut final_response = final_response.message;
        let token_usage = usage_for_response(
            provider_usage.or_else(|| final_response.usage.clone()),
            &final_messages,
            &[],
            &final_response,
        );
        if final_response.usage.is_none() {
            final_response.usage = Some(token_usage.clone());
        }
        append_usage_record_with_warning(
            &config.data_dir,
            &usage::UsageRecord {
                timestamp_unix: usage::now_unix(),
                provider: config.provider_name.clone(),
                model: config.provider_model.clone(),
                input_tokens: token_usage.input_tokens,
                output_tokens: token_usage.output_tokens,
                total_tokens: token_usage.total_tokens,
                cache_read_tokens: token_usage.cache_read_tokens,
                cache_write_tokens: token_usage.cache_write_tokens,
                source: token_usage.source.clone(),
            },
        );
        if !interactive || !final_was_rendered_live {
            render_assistant_response(&final_response, interactive, self.color_mode, &self.colors)?;
        }
        self.session.append_message(&final_response)?;
        self.messages.push(final_response);
        self.maybe_warn_context_pressure(
            self.stats().estimated_tokens,
            config.max_context_tokens,
            interactive,
        );
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

            let path_spec = ui_path_argument(&spec, &self.cwd);
            let resolved = builtin_tools::path::resolve_to_cwd(&path_spec, &self.cwd)?;
            let image = messages::image_from_path(&resolved)?;
            preview_attached_image(Some(&resolved), &image);
            self.pending_images.push(image);
            eprintln!("[image] attached {}", resolved.display());
            remove_if_ferrum_temp_image(&resolved);
        }
        Ok(())
    }

    async fn ensure_mcp(&mut self, config: &Config) -> Result<()> {
        let servers = active_mcp_servers(config);
        if self.mcp_enabled && self.mcp.is_none() && !servers.is_empty() {
            self.mcp = Some(mcp::McpManager::start(&servers).await?);
        }
        Ok(())
    }

    fn set_mcp_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.mcp_enabled == enabled {
            println!("MCP: {}", if enabled { "on" } else { "off" });
            return Ok(());
        }
        self.mcp_enabled = enabled;
        if !enabled {
            self.mcp = None;
        }
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
        let mut native_tools = builtin_tools::definitions();
        native_tools.extend(history_tool_definitions());
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
        let configured_servers = active_mcp_servers(config);
        let configured = configured_servers.len();
        let configured_enabled = configured_servers
            .iter()
            .filter(|server| server.enabled)
            .count();
        println!("MCP: {}", if self.mcp_enabled { "on" } else { "off" });
        println!("configured_servers: {configured}");
        println!("configured_enabled_servers: {configured_enabled}");
        if let Some(allow) = &config.mcp_server_allow {
            println!("server_filter: {}", allow.join(","));
        }
        println!("connected: {}", self.mcp.is_some());
        println!("native_tools: {}", native_tools.len());
        println!("mcp_tools_exposed: {}", mcp_tools.len());
        println!("total_tools_exposed: {}", exposed.len());
        println!("tool_schema_bytes: {}", tool_schema_bytes(&exposed));
        if !configured_servers.is_empty() {
            println!("servers:");
            for server in &configured_servers {
                println!("- {} enabled={}", server.name, server.enabled);
            }
        }
    }

    async fn execute_tool_batch(
        &mut self,
        tool_uses: Vec<(String, String, serde_json::Value)>,
        interactive: bool,
    ) -> Vec<ExecutedToolUse> {
        let can_parallelize = tool_uses
            .iter()
            .all(|(_, name, _)| self.is_parallel_safe_builtin_tool(name));
        let color_mode = self.color_mode;
        if can_parallelize && tool_uses.len() > 1 {
            let mut abort = ActiveTurnAbort::start(interactive);
            let cancel = Some(abort.token());
            let results = self
                .execute_parallel_builtin_tools(tool_uses, color_mode, self.colors.clone(), cancel)
                .await;
            abort.stop();
            return results;
        }
        self.execute_sequential_tools(tool_uses, color_mode, self.colors.clone(), interactive)
            .await
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
        color_mode: ColorMode,
        colors: ColorPalette,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Vec<ExecutedToolUse> {
        for (_, name, input) in &tool_uses {
            eprintln!();
            render_tool_call(name, input, self.diff_mode, color_mode, &colors);
        }

        let cwd = self.cwd.clone();
        let active_tool_names = self.active_tool_names.clone();
        let safety = self.safety;
        let mut handles = Vec::new();
        for (index, (id, name, input)) in tool_uses.into_iter().enumerate() {
            let cwd = cwd.clone();
            let active_tool_names = active_tool_names.clone();
            let cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                let started = Instant::now();
                let (content, is_error, aborted) = if cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    ("aborted".to_string(), true, true)
                } else if !active_tool_names.contains(&name) {
                    let content = if active_tool_names.is_empty() {
                        format!(
                            "Tool '{name}' is not available because tools are disabled (--no-tools)"
                        )
                    } else {
                        format!("Tool '{name}' is not in the active tool set")
                    };
                    (content, true, false)
                } else {
                    match builtin_tools::execute_with_cancel_and_safety(
                        &name, &input, &cwd, cancel, false, safety,
                    )
                    .await
                    {
                        Ok(output) => (output, false, false),
                        Err(error) if error.to_string() == "aborted" => {
                            ("aborted".to_string(), true, true)
                        }
                        Err(error) => (error.to_string(), true, false),
                    }
                };
                (
                    index,
                    ExecutedToolUse {
                        id,
                        name,
                        input,
                        content,
                        is_error,
                        aborted,
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
                        aborted: false,
                        duration_ms: 0,
                    },
                )),
            }
        }
        results.sort_by_key(|(index, _)| *index);
        let mut executed = Vec::new();
        for (_, result) in results {
            render_tool_result(
                &result.name,
                &result.content,
                result.is_error,
                color_mode,
                &colors,
            );
            emit_tool_metrics_if_enabled(&result);
            executed.push(result);
        }
        executed
    }

    async fn execute_sequential_tools(
        &mut self,
        tool_uses: Vec<(String, String, serde_json::Value)>,
        color_mode: ColorMode,
        colors: ColorPalette,
        interactive: bool,
    ) -> Vec<ExecutedToolUse> {
        let mut results = Vec::new();
        for (id, name, input) in tool_uses {
            eprintln!();
            render_tool_call(&name, &input, self.diff_mode, color_mode, &colors);
            let started = Instant::now();
            let mut abort = ActiveTurnAbort::start(interactive);
            let token = abort.token();
            let (content, is_error, aborted) = match self
                .execute_tool(&name, &input, interactive, Some(token))
                .await
            {
                Ok(output) => (output, false, false),
                Err(error) if error.to_string() == "aborted" => ("aborted".to_string(), true, true),
                Err(error) => (error.to_string(), true, false),
            };
            abort.stop();
            if aborted {
                eprintln!();
            }
            render_tool_result(&name, &content, is_error, color_mode, &colors);
            let result = ExecutedToolUse {
                id,
                name,
                input,
                content,
                is_error,
                aborted,
                duration_ms: started.elapsed().as_millis(),
            };
            emit_tool_metrics_if_enabled(&result);
            results.push(result);
        }
        results
    }

    async fn execute_tool(
        &mut self,
        name: &str,
        input: &serde_json::Value,
        interactive: bool,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<String> {
        if !self.active_tool_names.contains(name) {
            let message = if self.active_tool_names.is_empty() {
                format!("Tool '{name}' is not available because tools are disabled (--no-tools)")
            } else {
                format!("Tool '{name}' is not in the active tool set")
            };
            anyhow::bail!(message);
        }
        if matches!(name, "history_search" | "history_read") {
            return self.execute_history_tool(name, input);
        }
        if self.mcp_enabled
            && let Some(mcp) = &mut self.mcp
            && mcp.has_tool(name)
        {
            return mcp.call(name, input).await;
        }
        builtin_tools::execute_with_cancel_and_safety(
            name,
            input,
            &self.cwd,
            cancel,
            interactive,
            self.safety,
        )
        .await
    }

    fn execute_history_tool(&self, name: &str, input: &serde_json::Value) -> Result<String> {
        match name {
            "history_search" => {
                let query = required_history_str(input, "query")?.to_string();
                let literal = input
                    .get("literal")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                let ignore_case = input
                    .get("ignore_case")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                let limit = input
                    .get("limit")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(10) as usize;
                session::jsonl::search_history(
                    self.session.path(),
                    session::jsonl::HistorySearchOptions {
                        query,
                        literal,
                        ignore_case,
                        limit,
                    },
                )
            }
            "history_read" => {
                let offset = input
                    .get("offset")
                    .and_then(|value| value.as_u64())
                    .ok_or_else(|| anyhow::anyhow!("missing required integer field: offset"))?
                    as usize;
                let limit = input
                    .get("limit")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(20) as usize;
                Ok(session::jsonl::read_history(
                    self.session.path(),
                    offset,
                    limit,
                )?)
            }
            _ => unreachable!("unknown history tool: {name}"),
        }
    }

    fn message_count(&self) -> usize {
        self.messages.len()
    }

    fn stats(&self) -> SessionStats {
        let chars = self
            .messages
            .iter()
            .map(|message| message.text_content().chars().count())
            .sum::<usize>();
        let (estimated_tokens, context_source) = context_tokens_from_usage(&self.messages)
            .map(|tokens| (tokens, ContextTokenSource::UsagePlusEstimate))
            .unwrap_or_else(|| {
                let source = if has_compaction_boundary(&self.messages) {
                    ContextTokenSource::EstimateAfterCompaction
                } else {
                    ContextTokenSource::Estimate
                };
                (estimated_tokens_for_messages(&self.messages), source)
            });
        let file_bytes = fs::metadata(self.session.path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        SessionStats {
            messages: self.messages.len(),
            chars,
            estimated_tokens,
            context_source,
            file_bytes,
        }
    }

    fn maybe_warn_context_pressure(
        &mut self,
        estimated_tokens: usize,
        max_context_tokens: usize,
        interactive: bool,
    ) {
        let percent = context_usage_percent(estimated_tokens, max_context_tokens);
        let Some(bucket) = context_warning_bucket(percent) else {
            self.last_context_warning_bucket = None;
            return;
        };
        if self
            .last_context_warning_bucket
            .is_some_and(|last_bucket| bucket <= last_bucket)
        {
            return;
        }

        let message = context_pressure_message(percent, estimated_tokens, max_context_tokens);
        if interactive {
            render_status_notice(&message, self.color_mode, &self.colors);
        } else {
            eprintln!("{message}");
        }
        self.last_context_warning_bucket = Some(bucket);
    }

    fn remove_empty_session(&mut self) -> Result<()> {
        if self.session.remove_if_empty()? {
            self.last_session_list.clear();
        }
        Ok(())
    }

    fn refresh_runtime_context(&mut self, config: &Config) -> Result<()> {
        let message =
            messages::Message::text(messages::Role::System, runtime_context(config, &self.cwd)?);
        if self
            .messages
            .first()
            .is_some_and(|message| matches!(message.role, messages::Role::System))
        {
            self.messages[0] = message.clone();
        } else {
            self.messages.insert(0, message.clone());
        }
        self.session.append_message(&message)
    }

    fn set_title(&mut self, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            anyhow::bail!("session title must not be empty");
        }
        self.session.append_title(title)?;
        set_terminal_title(title)
    }

    fn visible_sessions(&self, config: &Config) -> Result<Vec<session::jsonl::SessionInfo>> {
        Ok(
            session::jsonl::list_sessions_for_cwd(&config.sessions_dir(), &self.cwd)?
                .into_iter()
                .filter(|session| {
                    session.message_count > 0
                        || session.path == *self.session.path()
                        || session.title != "(empty session)"
                })
                .collect(),
        )
    }

    fn list_sessions(&mut self, config: &Config) -> Result<()> {
        let sessions = self.visible_sessions(config)?;
        self.last_session_list = sessions.clone();
        if sessions.is_empty() {
            println!("No sessions found in {}", self.cwd.display());
            return Ok(());
        }
        println!("Recent sessions in {}\n", self.cwd.display());
        print_session_list(&sessions, self.session.path());
        println!("\nUse /sessions pick, /sessions del, or /sessions new");
        Ok(())
    }

    fn open_session_by_index(&mut self, config: &mut Config, index: usize) -> Result<()> {
        if self.last_session_list.is_empty() {
            anyhow::bail!("no session list is active. Run /sessions or /sessions pick first")
        }
        let Some(session) = self.last_session_list.get(index.saturating_sub(1)) else {
            anyhow::bail!("no session [{index}] in the active session list")
        };
        let path = session.path.clone();
        if path == *self.session.path() {
            println!("Already on session {}", path.display());
            return Ok(());
        }
        self.remove_empty_session()?;
        let _restored_tools =
            restore_session_preferences(config, &path, true, true, true, true, true)?;
        let next = Self::open_session(config, path)?;
        *self = next;
        print_current_session_header(self)?;
        Ok(())
    }

    fn delete_session_by_index(&mut self, index: usize) -> Result<()> {
        if self.last_session_list.is_empty() {
            anyhow::bail!("no session list is active. Run /sessions or /sessions del first")
        }
        let Some(session) = self.last_session_list.get(index.saturating_sub(1)) else {
            anyhow::bail!("no session [{index}] in the active session list")
        };
        let path = session.path.clone();
        if path == *self.session.path() {
            anyhow::bail!(
                "cannot delete the active session; switch sessions or start /sessions new first"
            )
        }
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        println!("deleted {}", path.display());
        self.last_session_list.retain(|entry| entry.path != path);
        Ok(())
    }

    fn new_session(&mut self, config: &Config) -> Result<()> {
        self.remove_empty_session()?;
        let next = Self::new(config)?;
        println!("started new session {}", next.session.path().display());
        *self = next;
        print_current_session_header(self)?;
        Ok(())
    }

    fn pick_session(&mut self, config: &mut Config) -> Result<()> {
        let mut query = String::new();
        loop {
            let all = self.visible_sessions(config)?;
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
                return self.open_session_by_index(config, input.parse()?);
            }
            query = input.to_string();
        }
    }

    fn delete_session_picker(&mut self, config: &Config) -> Result<()> {
        let mut query = String::new();
        loop {
            let all = self.visible_sessions(config)?;
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
            print!("\nDelete number, search text, or blank to cancel: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();
            if input.is_empty() {
                println!("cancelled");
                return Ok(());
            }
            if input.chars().all(|ch| ch.is_ascii_digit()) {
                let index: usize = input.parse()?;
                return self.delete_session_by_index(index);
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
        cancel: Option<Arc<AtomicBool>>,
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
        let split_index = avoid_orphan_tool_results(&conversation, split_index);
        let (to_summarize, recent) = conversation.split_at(split_index);
        if to_summarize.is_empty() {
            return Ok(CompactionOutcome::Skipped {
                before_tokens,
                after_tokens: before_tokens,
                reason: "conversation is already within recent-context budget".to_string(),
            });
        }

        let summary = compaction_summary_or_fallback(
            self.generate_compaction_summary(config, to_summarize, custom_instructions, cancel)
                .await,
            to_summarize,
            custom_instructions,
            force,
        )?;

        let summary_message = messages::Message::text(
            messages::Role::System,
            format!(
                "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{}\n</summary>",
                summary.trim()
            ),
        );

        let mut compacted_messages = system_messages;
        compacted_messages.push(summary_message.clone());
        compacted_messages.extend(recent.iter().cloned().map(clear_message_usage));
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
        cancel: Option<Arc<AtomicBool>>,
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
        let mut on_event = |_event: providers::StreamEvent| {};
        let response = provider
            .complete_streaming(
                &config.provider_model,
                &request_messages,
                &[],
                config.thinking,
                &mut on_event,
                cancel,
            )
            .await?;
        Ok(response.message.text_content())
    }
}

fn render_tool_call(
    name: &str,
    input: &serde_json::Value,
    diff_mode: DiffMode,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    eprintln!(
        "{}",
        colors.paint(ColorToken::Tool, color_mode, format!("[tool:{name}]"))
    );
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
        "edit" => render_edit_call(input, diff_mode, color_mode, colors),
        _ => {
            let rendered =
                serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
            eprintln!("args:\n{}", indent_block(&rendered));
        }
    }
}

fn render_edit_call(
    input: &serde_json::Value,
    diff_mode: DiffMode,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    eprintln!("path: {}", json_str(input, "path").unwrap_or("<missing>"));
    eprintln!("diff: {}", diff_mode.as_str());
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
        match diff_mode {
            DiffMode::Unified => render_unified_diff(old_text, new_text, 3, color_mode, colors),
            DiffMode::Compact => render_unified_diff(old_text, new_text, 1, color_mode, colors),
            DiffMode::Full => render_full_diff(old_text, new_text, color_mode, colors),
            DiffMode::Words => render_word_diff(old_text, new_text, color_mode, colors),
            DiffMode::SideBySide => {
                render_side_by_side_diff(old_text, new_text, color_mode, colors)
            }
        }
    }
}

fn render_unified_diff(
    old_text: &str,
    new_text: &str,
    context: usize,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    eprintln!(
        "{}",
        colors.paint(ColorToken::DiffMeta, color_mode, "--- old")
    );
    eprintln!(
        "{}",
        colors.paint(ColorToken::DiffMeta, color_mode, "+++ new")
    );
    let diff = TextDiff::from_lines(old_text, new_text);
    for group in diff.grouped_ops(context) {
        let old_start = group
            .first()
            .map(|op| op.old_range().start + 1)
            .unwrap_or(1);
        let new_start = group
            .first()
            .map(|op| op.new_range().start + 1)
            .unwrap_or(1);
        eprintln!(
            "{}",
            colors.paint(
                ColorToken::DiffHunk,
                color_mode,
                format!("@@ -{old_start} +{new_start} @@")
            )
        );
        for op in group {
            for change in diff.iter_changes(&op) {
                let (prefix, token) = match change.tag() {
                    ChangeTag::Delete => ("-", Some(ColorToken::DiffRemoved)),
                    ChangeTag::Insert => ("+", Some(ColorToken::DiffAdded)),
                    ChangeTag::Equal => (" ", None),
                };
                let text = change.to_string();
                if text.ends_with('\n') {
                    for line in text.split_inclusive('\n') {
                        let rendered = format!("{prefix}{line}");
                        if let Some(token) = token {
                            eprint!("{}", colors.paint(token, color_mode, rendered));
                        } else {
                            eprint!("{rendered}");
                        }
                    }
                } else {
                    let rendered = format!("{prefix}{text}");
                    if let Some(token) = token {
                        eprintln!("{}", colors.paint(token, color_mode, rendered));
                    } else {
                        eprintln!("{rendered}");
                    }
                    eprintln!("\\ No newline at end of line");
                }
            }
        }
    }
}

fn render_full_diff(old_text: &str, new_text: &str, color_mode: ColorMode, colors: &ColorPalette) {
    eprintln!(
        "{}",
        colors.paint(ColorToken::DiffRemoved, color_mode, "--- old")
    );
    if old_text.is_empty() {
        eprintln!("  [empty]");
    } else {
        let block = indent_block(old_text.trim_end_matches('\n'));
        for line in block.lines() {
            eprintln!(
                "{}",
                colors.paint(ColorToken::DiffRemoved, color_mode, line)
            );
        }
    }
    eprintln!(
        "{}",
        colors.paint(ColorToken::DiffAdded, color_mode, "+++ new")
    );
    if new_text.is_empty() {
        eprintln!("  [empty]");
    } else {
        let block = indent_block(new_text.trim_end_matches('\n'));
        for line in block.lines() {
            eprintln!("{}", colors.paint(ColorToken::DiffAdded, color_mode, line));
        }
    }
}

fn render_word_diff(old_text: &str, new_text: &str, color_mode: ColorMode, colors: &ColorPalette) {
    eprintln!("words:");
    let old_lines = old_text.lines().collect::<Vec<_>>();
    let new_lines = new_text.lines().collect::<Vec<_>>();
    let max_len = old_lines.len().max(new_lines.len());
    for index in 0..max_len {
        let old_line = old_lines.get(index).copied().unwrap_or("");
        let new_line = new_lines.get(index).copied().unwrap_or("");
        if old_line == new_line {
            eprintln!("  {old_line}");
            continue;
        }
        let diff = TextDiff::from_words(old_line, new_line);
        let mut rendered = String::new();
        for change in diff.iter_all_changes() {
            let token = change.to_string().replace(['\r', '\n'], "");
            match change.tag() {
                ChangeTag::Delete => {
                    let _ = write!(
                        rendered,
                        "{}",
                        colors.paint(ColorToken::DiffRemoved, color_mode, format!("[-{token}-]"))
                    );
                }
                ChangeTag::Insert => {
                    let _ = write!(
                        rendered,
                        "{}",
                        colors.paint(ColorToken::DiffAdded, color_mode, format!("{{+{token}+}}"))
                    );
                }
                ChangeTag::Equal => rendered.push_str(&token),
            }
        }
        eprintln!("  {rendered}");
    }
}

fn render_side_by_side_diff(
    old_text: &str,
    new_text: &str,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    let terminal_width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(60, 200);
    let column_width = (terminal_width.saturating_sub(9) / 2).max(20);
    eprintln!(
        "{}",
        colors.paint(
            ColorToken::DiffMeta,
            color_mode,
            format!(
                "{:<width$} | {:<width$}",
                "old",
                "new",
                width = column_width
            )
        )
    );
    eprintln!(
        "{}",
        colors.paint(
            ColorToken::DiffMeta,
            color_mode,
            format!(
                "{}-+-{}",
                "-".repeat(column_width),
                "-".repeat(column_width)
            )
        )
    );

    for row in side_by_side_rows(old_text, new_text) {
        let left = format!(
            "{}{:<width$}",
            row.left_marker,
            truncate_display(&row.left, column_width),
            width = column_width
        );
        let right = format!(
            "{}{:<width$}",
            row.right_marker,
            truncate_display(&row.right, column_width),
            width = column_width
        );
        let left_colored = if row.left_marker == "-" {
            colors.paint(ColorToken::DiffRemoved, color_mode, left)
        } else {
            left
        };
        let right_colored = if row.right_marker == "+" {
            colors.paint(ColorToken::DiffAdded, color_mode, right)
        } else {
            right
        };
        eprintln!("{} | {}", left_colored, right_colored);
    }
}

struct SideBySideRow {
    left_marker: &'static str,
    left: String,
    right_marker: &'static str,
    right: String,
}

fn side_by_side_rows(old_text: &str, new_text: &str) -> Vec<SideBySideRow> {
    let diff = TextDiff::from_lines(old_text, new_text);
    let mut rows = Vec::new();
    let mut pending_deletes = Vec::new();
    let mut pending_inserts = Vec::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                flush_side_by_side_changes(&mut rows, &mut pending_deletes, &mut pending_inserts);
                let line = trim_diff_line(change.to_string());
                rows.push(SideBySideRow {
                    left_marker: " ",
                    left: line.clone(),
                    right_marker: " ",
                    right: line,
                });
            }
            ChangeTag::Delete => pending_deletes.push(trim_diff_line(change.to_string())),
            ChangeTag::Insert => pending_inserts.push(trim_diff_line(change.to_string())),
        }
    }
    flush_side_by_side_changes(&mut rows, &mut pending_deletes, &mut pending_inserts);
    rows
}

fn flush_side_by_side_changes(
    rows: &mut Vec<SideBySideRow>,
    deletes: &mut Vec<String>,
    inserts: &mut Vec<String>,
) {
    let max_len = deletes.len().max(inserts.len());
    for index in 0..max_len {
        rows.push(SideBySideRow {
            left_marker: if index < deletes.len() { "-" } else { " " },
            left: deletes.get(index).cloned().unwrap_or_default(),
            right_marker: if index < inserts.len() { "+" } else { " " },
            right: inserts.get(index).cloned().unwrap_or_default(),
        });
    }
    deletes.clear();
    inserts.clear();
}

fn trim_diff_line(mut line: String) -> String {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    line
}

fn truncate_display(value: &str, width: usize) -> String {
    let max = width.saturating_sub(1);
    let mut chars = value.chars();
    let mut output = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

fn render_tool_result(
    name: &str,
    content: &str,
    is_error: bool,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    let display_error = is_error || bash_preview_indicates_failure(name, content);
    let status = if display_error { "error" } else { "ok" };
    let line_count = content.lines().count();
    let bytes = content.len();
    if is_error && let Some(reason) = blocked_tool_reason(name, content) {
        eprintln!(
            "{}",
            colors.paint(
                ColorToken::Warning,
                color_mode,
                format!("[tool:{name} blocked] {reason}")
            )
        );
    }
    let result_token = if display_error {
        ColorToken::Error
    } else {
        ColorToken::Success
    };
    eprintln!(
        "{}",
        colors.paint(
            result_token,
            color_mode,
            format!("[result:{name} {status}, {line_count} lines, {bytes} bytes]")
        )
    );
    let preview = truncate_chars(content.trim(), TOOL_PREVIEW_MAX_CHARS);
    if !preview.is_empty() {
        render_tool_preview(name, &preview, color_mode, colors);
        if content.chars().count() > TOOL_PREVIEW_MAX_CHARS {
            eprintln!(
                "{}",
                colors.paint(
                    ColorToken::Status,
                    color_mode,
                    "  [result truncated for display; full result kept in context]"
                )
            );
        }
    }
}

fn bash_preview_indicates_failure(name: &str, content: &str) -> bool {
    if !matches!(name, "bash" | "wait") {
        return false;
    }
    content.lines().any(|line| {
        line.strip_prefix("status: Some(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code != 0)
    }) || content.lines().any(|line| line == "timed_out: true")
}

fn bash_preview_stderr_token(name: &str, preview: &str) -> ColorToken {
    if bash_preview_indicates_failure(name, preview) {
        ColorToken::Error
    } else {
        ColorToken::ToolOutput
    }
}

fn render_tool_preview(name: &str, preview: &str, color_mode: ColorMode, colors: &ColorPalette) {
    if matches!(name, "bash" | "wait") && preview.contains("\nstderr:\n") {
        let mut in_stderr = false;
        let stderr_token = bash_preview_stderr_token(name, preview);
        for line in preview.lines() {
            if line == "stderr:" {
                in_stderr = true;
            }
            let token = if in_stderr {
                stderr_token
            } else {
                ColorToken::ToolOutput
            };
            eprintln!("{}", colors.paint(token, color_mode, format!("  {line}")));
        }
        return;
    }

    eprintln!(
        "{}",
        colors.paint(ColorToken::ToolOutput, color_mode, indent_block(preview))
    );
}

fn blocked_tool_reason<'a>(name: &str, content: &'a str) -> Option<&'a str> {
    let disabled =
        format!("Tool '{name}' is not available because tools are disabled (--no-tools)");
    let not_in_set = format!("Tool '{name}' is not in the active tool set");
    let denied = format!("Tool '{name}' is denied by config");
    let unavailable = format!("Tool '{name}' is not available");
    if content == disabled {
        Some("tools are disabled (--no-tools)")
    } else if content == not_in_set {
        Some("tool is not in the active tool set")
    } else if content == denied {
        Some("tool is denied by config")
    } else if content == unavailable {
        Some("tool is not available in this session")
    } else {
        None
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

fn print_session_title(title: &str, color_mode: ColorMode, colors: &ColorPalette) {
    println!();
    println!(
        "{} {title}",
        colors.paint(ColorToken::Highlight, color_mode, "title:")
    );
    render_hr(color_mode, colors);
}

fn set_terminal_title(title: &str) -> Result<()> {
    let title = title.replace(['\x1b', '\x07'], "");
    print!("\x1b]0;Ferrum: {title}\x07");
    io::stdout().flush()?;
    Ok(())
}

fn print_current_session_header(state: &AgentState) -> Result<()> {
    let info = session::jsonl::session_info(state.session.path())?
        .ok_or_else(|| anyhow::anyhow!("current session metadata unavailable"))?;
    set_terminal_title(&info.title)?;
    print_session_title(&info.title, state.color_mode, &state.colors);
    Ok(())
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
        let thinking = session.thinking.as_deref().unwrap_or("off");
        let diff_mode = session.diff_mode.as_deref().unwrap_or("unified");
        let mut provider_model = if thinking == "off" {
            format!("{provider}/{model}")
        } else {
            format!("{provider}/{model} think={thinking}")
        };
        if diff_mode != "unified" {
            provider_model.push_str(&format!(" diff={diff_mode}"));
        }
        let message_label = if session.archived_message_count > 0 {
            format!(
                "{} msgs +{} archived",
                session.message_count, session.archived_message_count
            )
        } else {
            format!("{} msgs", session.message_count)
        };
        let compaction_label = session
            .last_compaction_timestamp_ms
            .map(|timestamp| format!(" compacted={}", format_timestamp_ms(timestamp)))
            .unwrap_or_default();
        println!(
            "[{}] {marker} {:>4} {:<22} {:<28} {}{}",
            index + 1,
            age,
            truncate_chars(&message_label, 22).replace('\n', " "),
            truncate_chars(&provider_model, 28).replace('\n', " "),
            session.title,
            compaction_label
        );
    }
}

fn format_timestamp_ms(timestamp_ms: u64) -> String {
    let seconds = (timestamp_ms / 1000).min(i64::MAX as u64) as i64;
    let Ok(datetime) = time::OffsetDateTime::from_unix_timestamp(seconds) else {
        return timestamp_ms.to_string();
    };
    datetime
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| timestamp_ms.to_string())
}

fn print_usage_summary(period: usage::UsagePeriod, rows: &[usage::UsageSummaryRow]) {
    println!("usage: {}", period.label());
    if rows.is_empty() {
        println!("no usage records found");
        return;
    }
    println!(
        "{:<32} {:>4} {:>9} {:>10} {:>10} {:>8} {:>10}",
        "provider/model", "req", "exact/est/?", "input", "output", "cached", "total"
    );
    for row in rows {
        println!(
            "{:<32} {:>4} {:>9} {:>10} {:>10} {:>8} {:>10}",
            truncate_chars(&format!("{}/{}", row.provider, row.model), 32),
            row.summary.requests,
            format!(
                "{}/{}/{}",
                row.summary.provider_records,
                row.summary.estimated_records,
                row.summary.unknown_records
            ),
            format_token_count(row.summary.input_tokens),
            format_token_count(row.summary.output_tokens),
            format_token_count(row.summary.cache_read_tokens),
            format_token_count(row.summary.total_tokens),
        );
    }
}

fn format_token_count(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (index, ch) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push('_');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn print_recent_conversation_lines(
    messages: &[messages::Message],
    limit: usize,
    color_mode: ColorMode,
    colors: &ColorPalette,
) {
    let lines = recent_conversation_lines(messages, limit);
    if lines.is_empty() {
        return;
    }
    println!();
    println!(
        "{}",
        colors.paint(
            ColorToken::Highlight,
            color_mode,
            format!("Recent conversation ({}):", current_preview_timestamp())
        )
    );
    for line in lines {
        println!("{line}");
    }
    render_hr(color_mode, colors);
}

fn current_preview_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format_preview_timestamp_ms(now.as_millis().min(u128::from(u64::MAX)) as u64)
}

fn format_preview_timestamp_ms(timestamp_ms: u64) -> String {
    let seconds = (timestamp_ms / 1000).min(i64::MAX as u64) as i64;
    let Ok(datetime) = time::OffsetDateTime::from_unix_timestamp(seconds) else {
        return timestamp_ms.to_string();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        datetime.year(),
        u8::from(datetime.month()),
        datetime.day(),
        datetime.hour(),
        datetime.minute()
    )
}

fn recent_conversation_lines(messages: &[messages::Message], limit: usize) -> Vec<String> {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    for message in messages {
        let label = match message.role {
            messages::Role::User => "user",
            messages::Role::Assistant => "assistant",
            messages::Role::Tool => "tool",
            messages::Role::System => continue,
        };
        let content = message
            .display_text()
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>();
        if content.is_empty() {
            continue;
        }
        let mut block = Vec::with_capacity(content.len() + 1);
        block.push(format!("{label}:"));
        block.extend(content);
        blocks.push(block);
    }

    let limit = limit.max(1);
    let mut selected = Vec::new();
    let mut used = 0usize;
    for block in blocks.into_iter().rev() {
        let block_len = block.len();
        if selected.is_empty() && block_len > limit {
            let mut truncated = Vec::with_capacity(limit);
            truncated.push(block[0].clone());
            let keep = limit.saturating_sub(1);
            let start = block.len().saturating_sub(keep);
            truncated.extend(block.into_iter().skip(start));
            selected.push(truncated);
            break;
        }
        if used + block_len > limit {
            break;
        }
        used += block_len;
        selected.push(block);
    }
    selected.reverse();

    let mut lines = Vec::new();
    for (index, block) in selected.into_iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(block);
    }
    lines
}

fn print_session_preview(messages: &[messages::Message], limit: usize) {
    let preview = session_preview_lines(messages, limit);
    if preview.is_empty() {
        return;
    }
    let total = count_visible_session_messages(messages);
    println!();
    println!("Recent context ({}/{} messages):", preview.len(), total);
    for line in preview {
        println!("{}", line);
    }
}

fn count_visible_session_messages(messages: &[messages::Message]) -> usize {
    messages
        .iter()
        .filter(|message| {
            matches!(
                message.role,
                messages::Role::User | messages::Role::Assistant
            ) && !message.display_text().trim().is_empty()
        })
        .count()
}

fn session_preview_lines(messages: &[messages::Message], limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for message in messages.iter().rev() {
        match message.role {
            messages::Role::User | messages::Role::Assistant => {
                let text = message.display_text().replace('\n', " ");
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let label = match message.role {
                    messages::Role::User => "user",
                    messages::Role::Assistant => "assistant",
                    _ => unreachable!(),
                };
                lines.push(format!("{label}: {}", truncate_chars(text, 160)));
                if lines.len() >= limit {
                    break;
                }
            }
            messages::Role::System | messages::Role::Tool => {}
        }
    }
    lines.reverse();
    lines
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

fn is_context_overflow_error(error: &anyhow::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    let overflow_patterns = [
        "prompt is too long",
        "request_too_large",
        "input is too long for requested model",
        "exceeds the context window",
        "maximum context length",
        "input token count",
        "maximum prompt length",
        "reduce the length of the messages",
        "context window exceeds limit",
        "exceeded model token limit",
        "too large for model",
        "model_context_window_exceeded",
        "prompt too long",
        "context length exceeded",
        "context_length_exceeded",
        "too many tokens",
        "token limit exceeded",
    ];
    let non_overflow_patterns = ["rate limit", "too many requests", "throttling"];
    overflow_patterns
        .iter()
        .any(|pattern| text.contains(pattern))
        && !non_overflow_patterns
            .iter()
            .any(|pattern| text.contains(pattern))
}

fn should_auto_compact(estimated_tokens: usize, max_context_tokens: usize) -> bool {
    if max_context_tokens == 0 {
        return false;
    }
    if max_context_tokens > CONTEXT_RESERVE_TOKENS {
        return estimated_tokens >= max_context_tokens.saturating_sub(CONTEXT_RESERVE_TOKENS);
    }
    estimated_tokens.saturating_mul(100)
        >= max_context_tokens.saturating_mul(CONTEXT_AUTO_COMPACT_PERCENT)
}

fn context_usage_percent(estimated_tokens: usize, max_context_tokens: usize) -> usize {
    if max_context_tokens == 0 {
        return 0;
    }
    estimated_tokens.saturating_mul(100) / max_context_tokens
}

fn context_warning_bucket(percent: usize) -> Option<usize> {
    match percent {
        0..=74 => None,
        75..=84 => Some(((percent - CONTEXT_ADVISORY_PERCENT) / 5) * 5 + CONTEXT_ADVISORY_PERCENT),
        85..=91 => Some(((percent - CONTEXT_WARNING_PERCENT) / 3) * 3 + CONTEXT_WARNING_PERCENT),
        92..=94 => Some(percent),
        _ => Some(CONTEXT_AUTO_COMPACT_PERCENT),
    }
}

fn context_pressure_message(
    percent: usize,
    estimated_tokens: usize,
    max_context_tokens: usize,
) -> String {
    if percent >= CONTEXT_CRITICAL_PERCENT {
        format!(
            "[session] context {percent}% used ({estimated_tokens}/{max_context_tokens} estimated tokens); auto-compact will run at {CONTEXT_AUTO_COMPACT_PERCENT}%"
        )
    } else if percent >= CONTEXT_WARNING_PERCENT {
        format!(
            "[session] context {percent}% used ({estimated_tokens}/{max_context_tokens} estimated tokens); auto-compact is getting close"
        )
    } else {
        format!(
            "[session] context {percent}% used ({estimated_tokens}/{max_context_tokens} estimated tokens); consider /compact to control the summary point"
        )
    }
}

#[cfg(test)]
mod context_pressure_tests {
    use super::*;

    #[test]
    fn detects_failed_bash_preview_status() {
        assert!(bash_preview_indicates_failure(
            "bash",
            "status: Some(1)\ntimed_out: false\nstdout:\n\nstderr:\nnope"
        ));
        assert!(!bash_preview_indicates_failure(
            "bash",
            "status: Some(0)\ntimed_out: false\nstdout:\nok\nstderr:\n"
        ));
        assert!(!bash_preview_indicates_failure(
            "grep",
            "status: Some(1)\ntimed_out: false"
        ));
    }

    #[test]
    fn colors_bash_stderr_as_error_only_on_failure() {
        assert_eq!(
            bash_preview_stderr_token(
                "bash",
                "status: Some(0)\ntimed_out: false\nstdout:\nok\nstderr:\nFinished build\n"
            ),
            ColorToken::ToolOutput
        );
        assert_eq!(
            bash_preview_stderr_token(
                "bash",
                "status: Some(1)\ntimed_out: false\nstdout:\n\nstderr:\nfailed\n"
            ),
            ColorToken::Error
        );
        assert_eq!(
            bash_preview_stderr_token("bash", "status: Some(0)\ntimed_out: true\nstderr:\nkilled"),
            ColorToken::Error
        );
    }

    #[test]
    fn detects_chafa_pixel_formats_for_common_terminals() {
        assert_eq!(
            chafa_pixel_format_for_env(Some("xterm-ghostty"), Some("ghostty"), false, false),
            Some("kitty")
        );
        assert_eq!(
            chafa_pixel_format_for_env(Some("xterm-kitty"), None, true, false),
            Some("kitty")
        );
        assert_eq!(
            chafa_pixel_format_for_env(None, Some("iTerm.app"), false, false),
            Some("iterm")
        );
        assert_eq!(
            chafa_pixel_format_for_env(Some("foot"), None, false, false),
            Some("sixels")
        );
        assert_eq!(
            chafa_pixel_format_for_env(Some("xterm-256color"), None, false, false),
            None
        );
    }

    #[test]
    fn completes_sessions_subcommands() {
        let temp = tempfile::tempdir().unwrap();
        let helper = FerrumLineHelper::new(&[], &test_config(temp.path().to_path_buf()));
        let history = DefaultHistory::default();
        let ctx = rustyline::Context::new(&history);

        let line = " /sessions p";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();

        assert_eq!(start, line.len() - 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "pick");
    }

    #[test]
    fn completes_known_subcommands_and_modes() {
        let temp = tempfile::tempdir().unwrap();
        let palette_dir = temp.path().join("color-palettes");
        std::fs::create_dir_all(&palette_dir).unwrap();
        std::fs::write(palette_dir.join("catppuccin.toml"), "prompt = \"blue\"\n").unwrap();
        let helper = FerrumLineHelper::new(&[], &test_config(temp.path().to_path_buf()));

        let history = DefaultHistory::default();
        let ctx = rustyline::Context::new(&history);

        assert_completion(&helper, &ctx, "/colors a", "auto");
        assert_completion(&helper, &ctx, "/palette cat", "catppuccin");
        let (_start, candidates) = helper
            .complete("/palette ", "/palette ".len(), &ctx)
            .unwrap();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.replacement == "catppuccin")
        );
        assert_completion(&helper, &ctx, "/mcp l", "list");
    }

    #[test]
    fn completes_skill_space_invocation() {
        let temp = tempfile::tempdir().unwrap();
        let skill = skills::Skill {
            name: "ferrum-test".to_string(),
            description: "test skill".to_string(),
            path: temp.path().join("SKILL.md"),
            dir: temp.path().to_path_buf(),
        };
        let helper = FerrumLineHelper::new(&[skill], &test_config(temp.path().to_path_buf()));
        let history = DefaultHistory::default();
        let ctx = rustyline::Context::new(&history);

        assert_completion(&helper, &ctx, "/skill ferr", "ferrum-test");
        assert_completion(&helper, &ctx, "/skill:ferr", "ferrum-test");
    }

    #[test]
    fn does_not_insert_command_hint_after_trailing_space() {
        let temp = tempfile::tempdir().unwrap();
        let palette_dir = temp.path().join("color-palettes");
        std::fs::create_dir_all(&palette_dir).unwrap();
        std::fs::write(palette_dir.join("catppuccin.toml"), "prompt = \"blue\"\n").unwrap();
        let helper = FerrumLineHelper::new(&[], &test_config(temp.path().to_path_buf()));
        let history = DefaultHistory::default();
        let ctx = rustyline::Context::new(&history);

        assert_eq!(
            helper.hint("/sessions", "/sessions".len(), &ctx),
            Some(" pick | del | new".to_string())
        );
        assert_eq!(helper.hint("/sessions ", "/sessions ".len(), &ctx), None);
        assert_eq!(
            helper.hint("/palette", "/palette".len(), &ctx),
            Some(" <name>  (/palettes to list)".to_string())
        );
        assert!(
            helper
                .hint("/palette ", "/palette ".len(), &ctx)
                .unwrap()
                .contains("catppuccin")
        );
        assert_eq!(
            helper.hint("/palette cat", "/palette cat".len(), &ctx),
            Some("ppuccin".to_string())
        );
    }

    #[test]
    fn history_input_sanitizes_image_payloads_and_temp_paths() {
        let temp_path = ferrum_temp_dir()
            .unwrap()
            .join("ferrum-clipboard-secret.png");
        let input = format!("see data:image/png;base64,AAAA and {}", temp_path.display());
        let sanitized = sanitize_history_input(&input);
        assert!(sanitized.contains("[image omitted]"));
        assert!(sanitized.contains("[clipboard image]"));
        assert!(!sanitized.contains("base64"));
        assert!(!sanitized.contains("ferrum-clipboard-secret"));
    }

    #[test]
    fn history_file_permissions_are_private() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.txt");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        prepare_history_file(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn private_temp_image_files_are_random_and_private() {
        let first = write_private_temp_file("ferrum-test-", ".png", b"one").unwrap();
        let second = write_private_temp_file("ferrum-test-", ".png", b"one").unwrap();
        assert_ne!(first, second);
        assert!(is_ferrum_temp_image_path(&first));
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        remove_if_ferrum_temp_image(&first);
        remove_if_ferrum_temp_image(&second);
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[test]
    fn slash_command_completion_ignores_arguments_without_specific_completer() {
        let temp = tempfile::tempdir().unwrap();
        let helper = FerrumLineHelper::new(&[], &test_config(temp.path().to_path_buf()));
        let history = DefaultHistory::default();
        let ctx = rustyline::Context::new(&history);

        let (_start, candidates) = helper.complete("/title p", "/title p".len(), &ctx).unwrap();

        assert!(candidates.is_empty());
    }

    #[test]
    fn recent_conversation_lines_include_user_assistant_and_tool() {
        let messages = vec![
            messages::Message::text(messages::Role::System, "runtime"),
            messages::Message::text(messages::Role::User, "hello\nagain"),
            messages::Message::text(messages::Role::Assistant, "answer"),
            messages::Message::text(messages::Role::Tool, "tool line 1\ntool line 2"),
        ];

        assert_eq!(
            recent_conversation_lines(&messages, 6),
            vec![
                "assistant:".to_string(),
                "  answer".to_string(),
                "".to_string(),
                "tool:".to_string(),
                "  tool line 1".to_string(),
                "  tool line 2".to_string(),
            ]
        );
    }

    #[test]
    fn restored_session_tools_do_not_limit_new_default_tools() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = session::JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            Some(vec!["read".to_string(), "bash".to_string()]),
        )
        .unwrap();
        session
            .append_tools(&["read".to_string(), "bash".to_string()])
            .unwrap();
        let path = session.path().clone();
        drop(session);
        let mut config = test_config(temp.path().to_path_buf());

        let restored =
            restore_session_preferences(&mut config, &path, true, true, true, true, true).unwrap();
        let tools = resolve_available_tools(builtin_tools::definitions(), &config).unwrap();
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(restored, Some(vec!["read".to_string(), "bash".to_string()]));
        assert_eq!(config.tool_selection, None);
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"wait"));
    }

    #[test]
    fn wait_is_hidden_when_bash_is_denied() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        config.tools_deny = vec!["bash".to_string()];

        let tools = resolve_available_tools(builtin_tools::definitions(), &config).unwrap();
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert!(!names.contains(&"bash"));
        assert!(!names.contains(&"wait"));
    }

    #[test]
    fn explicit_wait_selection_requires_bash_availability() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        config.tool_selection = Some(ToolSelection::List(vec!["wait".to_string()]));

        let tools = resolve_available_tools(builtin_tools::definitions(), &config).unwrap();
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.is_empty());
    }

    #[test]
    fn default_prompt_lists_all_interactive_commands() {
        let prompt = default_system_prompt_template();
        for command in [
            "/help",
            "/version",
            "/session",
            "/title [text]",
            "/sessions del",
            "/skill <name> [args]",
            "/skill:<name> [args]",
            "/usage [day|week|month]",
            "/mcp [on|off|status|list]",
            "/colors [auto|on|off]",
            "/palette [name]",
            "/palettes",
            "/image-paste",
            "/paste-image",
            "/exit",
        ] {
            assert!(prompt.contains(command), "missing {command}");
        }
        assert!(!prompt.contains("/sessions <number|id-prefix|path>"));
    }

    #[test]
    fn context_usage_ignores_pre_compaction_assistant_usage() {
        let messages = vec![
            assistant_with_usage(237_351),
            messages::Message::text(
                messages::Role::System,
                "The conversation history before this point was compacted into the following summary:\n\n<summary>short</summary>",
            ),
            messages::Message::text(messages::Role::User, "recent request"),
        ];

        assert_eq!(context_tokens_from_usage(&messages), None);
        assert_eq!(
            estimated_tokens_for_messages(&messages),
            messages
                .iter()
                .map(estimated_tokens_for_message)
                .sum::<usize>()
        );
    }

    #[test]
    fn context_usage_uses_post_compaction_assistant_usage() {
        let messages = vec![
            assistant_with_usage(237_351),
            messages::Message::text(
                messages::Role::System,
                "The conversation history before this point was compacted into the following summary:\n\n<summary>short</summary>",
            ),
            assistant_with_usage(12_000),
            messages::Message::text(messages::Role::User, "recent request"),
        ];

        let trailing = estimated_tokens_for_message(messages.last().unwrap());
        assert_eq!(
            context_tokens_from_usage(&messages),
            Some(12_000 + trailing)
        );
    }

    #[test]
    fn live_render_counts_thinking_as_rendered_content() {
        let mut render = LiveRenderState::new(ColorMode::Off, ColorPalette::default());
        render.thinking_started = true;

        assert!(render.started());
        assert!(!render.text_started);
    }

    #[test]
    fn image_blocks_have_pessimistic_context_cost() {
        let image = messages::Message {
            role: messages::Role::User,
            content: vec![messages::ContentBlock::Image {
                mime_type: "image/png".to_string(),
                data_base64: "A".repeat(4 * 1024 * 1024),
                sha256: "hash".to_string(),
                source: "test".to_string(),
            }],
            usage: None,
        };

        assert!(estimated_tokens_for_message(&image) > IMAGE_BASE_TOKEN_ESTIMATE);
        assert!(estimated_tokens_for_message(&image) > 10_000);
    }

    #[test]
    fn large_image_turn_triggers_context_pressure_before_provider_rejects() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path().to_path_buf());
        let mut state = AgentState::new(&config).unwrap();
        state.messages.push(messages::Message {
            role: messages::Role::User,
            content: vec![
                messages::ContentBlock::Text {
                    text: "analyze this".to_string(),
                },
                messages::ContentBlock::Image {
                    mime_type: "image/png".to_string(),
                    data_base64: "A".repeat(4 * 1024 * 1024),
                    sha256: "hash".to_string(),
                    source: "test".to_string(),
                },
            ],
            usage: None,
        });

        let stats = state.stats();

        assert!(should_auto_compact(
            stats.estimated_tokens,
            config.max_context_tokens
        ));
    }

    #[test]
    fn loaded_compaction_summary_is_context_boundary() {
        let messages = vec![
            assistant_with_usage(237_351),
            messages::Message::text(
                messages::Role::System,
                "Conversation summary from previous compaction:\nshort",
            ),
        ];

        assert_eq!(context_tokens_from_usage(&messages), None);
        assert!(has_compaction_boundary(&messages));
    }

    #[test]
    fn estimate_after_compaction_context_source_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path().to_path_buf());
        let mut state = AgentState::new(&config).unwrap();
        state.messages = vec![
            assistant_with_usage(237_351),
            messages::Message::text(
                messages::Role::System,
                "The conversation history before this point was compacted into the following summary:\n\n<summary>short</summary>",
            ),
            messages::Message::text(messages::Role::User, "recent request"),
        ];

        let stats = state.stats();

        assert_eq!(stats.context_source.as_str(), "estimate_after_compaction");
        assert_eq!(
            stats.estimated_tokens,
            estimated_tokens_for_messages(&state.messages)
        );
    }

    #[test]
    fn clear_message_usage_removes_stale_retained_usage() {
        let retained = assistant_with_usage(237_351);
        let cleared = clear_message_usage(retained);

        assert!(cleared.usage.is_none());
    }

    #[test]
    fn compacted_retained_messages_do_not_reuse_pre_compaction_usage() {
        let compacted = vec![
            messages::Message::text(
                messages::Role::System,
                "The conversation history before this point was compacted into the following summary:\n\n<summary>short</summary>",
            ),
            clear_message_usage(assistant_with_usage(237_351)),
            messages::Message::text(messages::Role::User, "recent request"),
        ];

        assert_eq!(context_tokens_from_usage(&compacted), None);
    }

    #[test]
    fn buckets_context_pressure_by_cadence() {
        assert_eq!(context_warning_bucket(74), None);
        assert_eq!(context_warning_bucket(75), Some(75));
        assert_eq!(context_warning_bucket(79), Some(75));
        assert_eq!(context_warning_bucket(80), Some(80));
        assert_eq!(context_warning_bucket(84), Some(80));
        assert_eq!(context_warning_bucket(85), Some(85));
        assert_eq!(context_warning_bucket(87), Some(85));
        assert_eq!(context_warning_bucket(88), Some(88));
        assert_eq!(context_warning_bucket(91), Some(91));
        assert_eq!(context_warning_bucket(92), Some(92));
        assert_eq!(context_warning_bucket(94), Some(94));
        assert_eq!(context_warning_bucket(95), Some(95));
    }

    #[test]
    fn auto_compacts_before_full_context() {
        assert!(!should_auto_compact(94, 100));
        assert!(should_auto_compact(95, 100));
        assert!(!should_auto_compact(183_615, 200_000));
        assert!(should_auto_compact(183_616, 200_000));
    }

    #[test]
    fn usage_percent_is_floor_percent() {
        assert_eq!(context_usage_percent(149, 200), 74);
        assert_eq!(context_usage_percent(150, 200), 75);
    }

    #[test]
    fn forced_compaction_uses_fallback_for_empty_model_summary() {
        let messages = vec![messages::Message::text(messages::Role::User, "old context")];

        let summary =
            compaction_summary_or_fallback(Ok("   ".to_string()), &messages, None, true).unwrap();

        assert!(summary.contains("## Goal"));
        assert!(summary.contains("local fallback summary"));
        assert!(summary.contains("old context"));
    }

    #[test]
    fn manual_compaction_still_reports_empty_model_summary() {
        let messages = vec![messages::Message::text(messages::Role::User, "old context")];

        let error =
            compaction_summary_or_fallback(Ok("".to_string()), &messages, None, false).unwrap_err();

        assert_eq!(error.to_string(), "compaction summary was empty");
    }

    #[test]
    fn forced_compaction_uses_fallback_for_model_error() {
        let messages = vec![messages::Message::text(messages::Role::User, "old context")];

        let summary = compaction_summary_or_fallback(
            Err(anyhow::anyhow!("provider failed")),
            &messages,
            Some("token plans"),
            true,
        )
        .unwrap();

        assert!(summary.contains("local fallback summary"));
        assert!(summary.contains("User compaction focus: token plans"));
    }

    #[test]
    fn forced_fallback_summary_is_bounded() {
        let messages = (0..200)
            .map(|index| {
                messages::Message::text(
                    messages::Role::User,
                    format!("message {index} {}", "x".repeat(1_000)),
                )
            })
            .collect::<Vec<_>>();

        let summary = compaction_summary_or_fallback(
            Err(anyhow::anyhow!("provider failed")),
            &messages,
            None,
            true,
        )
        .unwrap();

        assert!(summary.len() < LOCAL_COMPACTION_SUMMARY_MAX_CHARS + 2_000);
        assert!(summary.contains("omitted from local fallback summary"));
    }

    #[test]
    fn compaction_split_drops_orphan_tool_results_from_recent_context() {
        let messages = vec![
            messages::Message::text(messages::Role::User, "old question"),
            messages::Message {
                role: messages::Role::Assistant,
                content: vec![messages::ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"path": "a.txt"}),
                }],
                usage: None,
            },
            messages::Message {
                role: messages::Role::Tool,
                content: vec![messages::ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "result".to_string(),
                    is_error: false,
                }],
                usage: None,
            },
            messages::Message::text(messages::Role::User, "new question"),
        ];

        assert_eq!(avoid_orphan_tool_results(&messages, 2), 3);
        assert_eq!(avoid_orphan_tool_results(&messages, 3), 3);
    }

    #[test]
    fn renders_external_system_prompt_template() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path().to_path_buf());
        let cwd = std::path::Path::new("/tmp/work");
        let rendered = render_system_prompt_template(
            "model={{model}} provider_model={{provider_model}} cwd={{cwd}} max={{max_context_tokens}}",
            &config,
            cwd,
        );

        assert_eq!(
            rendered,
            "model=alias provider_model=actual-model cwd=/tmp/work max=1234"
        );
    }

    #[test]
    fn restore_session_preferences_respects_provider_model_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let session = session::JsonlSession::create(
            temp.path().to_path_buf(),
            Some("fake".to_string()),
            Some("session-model".to_string()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let path = session.path().clone();
        drop(session);
        let mut config = test_config(temp.path().to_path_buf());
        config.model = "cli-model".to_string();
        config.provider_model = "cli-model".to_string();

        restore_session_preferences(&mut config, &path, true, true, true, false, false).unwrap();

        assert_eq!(config.provider_name, "fake");
        assert_eq!(config.model, "cli-model");
        assert_eq!(config.provider_model, "cli-model");
    }

    #[test]
    fn runtime_context_uses_config_system_md_when_present() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("system.md"),
            "custom prompt for {{model}} using {{provider_model}}",
        )
        .unwrap();
        let config = test_config(temp.path().to_path_buf());
        let rendered = runtime_context(&config, std::path::Path::new("/tmp/work")).unwrap();

        assert_eq!(rendered, "custom prompt for alias using actual-model");
    }

    #[test]
    fn refresh_runtime_context_updates_model_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        let mut state = AgentState::new(&config).unwrap();
        config.model = "new-model".to_string();
        config.provider_model = "new-provider-model".to_string();

        state.refresh_runtime_context(&config).unwrap();

        let text = state.messages[0].text_content();
        assert!(text.contains("model: new-model"));
        assert!(text.contains("provider_model: new-provider-model"));
    }

    #[test]
    fn resume_without_matching_session_creates_new_session() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());

        let state =
            AgentState::resume_ref(&mut config, None, true, true, true, true, true).unwrap();

        assert_eq!(state.cwd, cwd);
        assert!(state.session.path().exists());
        assert!(state.session.path().starts_with(config.sessions_dir()));
    }

    fn assert_completion(
        helper: &FerrumLineHelper,
        ctx: &rustyline::Context<'_>,
        line: &str,
        expected: &str,
    ) {
        let (_start, candidates) = helper.complete(line, line.len(), ctx).unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, expected);
    }

    fn assistant_with_usage(total_tokens: u64) -> messages::Message {
        messages::Message {
            role: messages::Role::Assistant,
            content: vec![messages::ContentBlock::Text {
                text: "assistant response".to_string(),
            }],
            usage: Some(messages::TokenUsage {
                input_tokens: Some(total_tokens.saturating_sub(1)),
                output_tokens: Some(1),
                total_tokens: Some(total_tokens),
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                source: "test".to_string(),
            }),
        }
    }

    #[test]
    fn absolute_path_leading_prompt_is_not_slash_command() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!should_handle_as_command(
            "/tmp/foo failed, explain why",
            temp.path()
        ));
    }

    #[test]
    fn unknown_slash_word_is_user_message() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!should_handle_as_command(
            "/not-a-command do something",
            temp.path()
        ));
        assert!(should_handle_as_command("/help", temp.path()));
    }

    #[test]
    fn ui_layer_can_strip_paste_marker_for_image_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("image.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nminimal").unwrap();
        let input = format!("look @{}", path.display());

        let (prompt, images) = extract_pasted_images(&input, temp.path());

        assert_eq!(prompt, "look");
        assert_eq!(images, vec![path.display().to_string()]);
    }

    #[test]
    fn pasted_quoted_image_path_attaches() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("image with spaces.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nminimal").unwrap();
        let input = format!("explain '{}' please", path.display());

        let (prompt, images) = extract_pasted_images(&input, temp.path());

        assert_eq!(prompt, "explain please");
        assert_eq!(images, vec![path.display().to_string()]);
    }

    #[test]
    fn file_url_with_percent_spaces_attaches() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("image with spaces.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nminimal").unwrap();
        let url = format!("file://{}", path.display()).replace(' ', "%20");

        let (prompt, images) = extract_pasted_images(&format!("look {url}"), temp.path());

        assert_eq!(prompt, "look");
        assert_eq!(images, vec![url]);
    }

    #[tokio::test]
    async fn manual_compaction_summary_honors_precancelled_token() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        config.max_context_tokens = 2;
        let mut state = AgentState::new(&config).unwrap();
        state
            .messages
            .push(messages::Message::text(messages::Role::User, "old message"));
        state.messages.push(messages::Message::text(
            messages::Role::Assistant,
            "old response",
        ));
        state.messages.push(messages::Message::text(
            messages::Role::User,
            "recent message",
        ));
        let cancel = Arc::new(AtomicBool::new(true));

        let error = state
            .compact(&config, None, true, Some(cancel))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "aborted");
    }

    #[tokio::test]
    async fn parallel_builtin_batch_marks_precancelled_tools_aborted() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path().to_path_buf());
        let state = AgentState::new(&config).unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let tool_uses = vec![
            (
                "call_1".to_string(),
                "read".to_string(),
                serde_json::json!({"path":"Cargo.toml"}),
            ),
            (
                "call_2".to_string(),
                "ls".to_string(),
                serde_json::json!({"path":"."}),
            ),
        ];

        let results = state
            .execute_parallel_builtin_tools(
                tool_uses,
                ColorMode::Auto,
                ColorPalette::default(),
                Some(cancel),
            )
            .await;

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.aborted));
        assert!(results.iter().all(|result| result.is_error));
    }

    #[test]
    fn active_mcp_servers_excludes_disabled_servers() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        config.mcp_servers = vec![
            crate::config::McpServerConfig {
                name: "enabled".to_string(),
                command: "true".to_string(),
                args: Vec::new(),
                enabled: true,
            },
            crate::config::McpServerConfig {
                name: "disabled".to_string(),
                command: "true".to_string(),
                args: Vec::new(),
                enabled: false,
            },
        ];

        let servers = active_mcp_servers(&config);

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "enabled");
    }

    #[test]
    fn active_mcp_servers_applies_allow_list_after_enabled_filter() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config(temp.path().to_path_buf());
        config.mcp_server_allow = Some(vec!["disabled".to_string()]);
        config.mcp_servers = vec![
            crate::config::McpServerConfig {
                name: "enabled".to_string(),
                command: "true".to_string(),
                args: Vec::new(),
                enabled: true,
            },
            crate::config::McpServerConfig {
                name: "disabled".to_string(),
                command: "true".to_string(),
                args: Vec::new(),
                enabled: false,
            },
        ];

        let servers = active_mcp_servers(&config);

        assert!(servers.is_empty());
    }

    fn test_config(config_dir: std::path::PathBuf) -> Config {
        Config {
            data_dir: config_dir.clone(),
            config_dir,
            model: "alias".to_string(),
            provider_model: "actual-model".to_string(),
            provider_name: "fake".to_string(),
            provider: crate::config::ProviderConfig::Fake,
            providers: std::collections::BTreeMap::new(),
            models: std::collections::BTreeMap::new(),
            offline: false,
            max_context_tokens: 1234,
            base_max_context_tokens: 1234,
            max_tool_rounds: 0,
            thinking: crate::config::ThinkingLevel::Off,
            mcp_enabled: true,
            mcp_server_allow: None,
            color_mode: crate::config::ColorMode::Auto,
            colors: ColorPalette::default(),
            diff_mode: crate::config::DiffMode::Unified,
            safety: SafetyLevel::Medium,
            tools_allow: None,
            tools_deny: Vec::new(),
            tool_selection: None,
            mcp_servers: Vec::new(),
        }
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

fn avoid_orphan_tool_results(messages: &[messages::Message], split_index: usize) -> usize {
    let mut adjusted = split_index;
    while adjusted < messages.len() && message_has_tool_result(&messages[adjusted]) {
        adjusted += 1;
    }
    adjusted
}

fn message_has_tool_result(message: &messages::Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, messages::ContentBlock::ToolResult { .. }))
}

fn clear_message_usage(mut message: messages::Message) -> messages::Message {
    message.usage = None;
    message
}

fn estimated_tokens_for_messages(messages: &[messages::Message]) -> usize {
    messages
        .iter()
        .map(estimated_tokens_for_message)
        .sum::<usize>()
}

fn context_tokens_from_usage(messages: &[messages::Message]) -> Option<usize> {
    let usage_start = latest_compaction_boundary(messages).map_or(0, |index| index + 1);
    let (index, usage) = messages
        .iter()
        .enumerate()
        .skip(usage_start)
        .rev()
        .find_map(|(index, message)| {
            matches!(message.role, messages::Role::Assistant)
                .then_some(message.usage.as_ref())
                .flatten()
                .and_then(|usage| usage.context_tokens().map(|tokens| (index, tokens)))
        })?;
    let trailing = messages
        .iter()
        .skip(index + 1)
        .map(estimated_tokens_for_message)
        .sum::<usize>();
    Some((usage as usize).saturating_add(trailing))
}

fn has_compaction_boundary(messages: &[messages::Message]) -> bool {
    latest_compaction_boundary(messages).is_some()
}

fn latest_compaction_boundary(messages: &[messages::Message]) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| message_is_compaction_summary(message).then_some(index))
}

fn message_is_compaction_summary(message: &messages::Message) -> bool {
    matches!(message.role, messages::Role::System)
        && message.content.iter().any(|block| match block {
            messages::ContentBlock::Text { text } => {
                text.starts_with("The conversation history before this point was compacted into the following summary:")
                    || text.starts_with("Conversation summary from previous compaction:")
            }
            _ => false,
        })
}

const IMAGE_BASE_TOKEN_ESTIMATE: usize = 1_200;
const IMAGE_BYTES_PER_TOKEN_ESTIMATE: usize = 512;
const IMAGE_MEGABYTE_TOKEN_ESTIMATE: usize = 1_000;

fn estimated_tokens_for_message(message: &messages::Message) -> usize {
    message
        .content
        .iter()
        .map(estimated_tokens_for_content_block)
        .sum::<usize>()
}

fn estimated_tokens_for_content_block(block: &messages::ContentBlock) -> usize {
    match block {
        messages::ContentBlock::Text { text } | messages::ContentBlock::Thinking { text, .. } => {
            text.chars().count().div_ceil(4)
        }
        messages::ContentBlock::ToolUse { name, input, .. } => name
            .chars()
            .count()
            .saturating_add(input.to_string().chars().count())
            .div_ceil(4),
        messages::ContentBlock::ToolResult { content, .. } => content.chars().count().div_ceil(4),
        messages::ContentBlock::Image { data_base64, .. } => {
            estimated_tokens_for_image(data_base64)
        }
    }
}

fn estimated_tokens_for_image(data_base64: &str) -> usize {
    let approx_bytes = data_base64.len().saturating_mul(3) / 4;
    let size_tokens = approx_bytes.div_ceil(IMAGE_BYTES_PER_TOKEN_ESTIMATE);
    let megabyte_tokens = approx_bytes
        .div_ceil(1024 * 1024)
        .saturating_mul(IMAGE_MEGABYTE_TOKEN_ESTIMATE);
    IMAGE_BASE_TOKEN_ESTIMATE
        .saturating_add(size_tokens)
        .saturating_add(megabyte_tokens)
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

fn compaction_summary_or_fallback(
    generated: Result<String>,
    messages: &[messages::Message],
    custom_instructions: Option<&str>,
    force: bool,
) -> Result<String> {
    match generated {
        Ok(summary) if !summary.trim().is_empty() => Ok(summary),
        Ok(_) if force => {
            eprintln!(
                "[session] model compaction returned an empty summary; using local fallback summary"
            );
            Ok(local_compaction_summary(messages, custom_instructions))
        }
        Ok(_) => anyhow::bail!("compaction summary was empty"),
        Err(error) if error.to_string() == "aborted" => Err(error),
        Err(error) if force => {
            eprintln!("[session] model compaction failed: {error}; using local fallback summary");
            Ok(local_compaction_summary(messages, custom_instructions))
        }
        Err(error) => Err(error).context("model compaction failed"),
    }
}

fn local_compaction_summary(
    messages: &[messages::Message],
    custom_instructions: Option<&str>,
) -> String {
    let mut summary = String::new();
    summary.push_str("## Goal\n(unknown; model summarization failed)\n\n");
    summary.push_str("## Constraints & Preferences\n- (unknown)\n\n");
    summary.push_str("## Progress\n### Done\n");
    let mut omitted = 0usize;
    for message in messages {
        let text = message_text_for_compaction(message);
        if text.trim().is_empty() {
            continue;
        }
        let entry = format!(
            "- {:?}: {}\n",
            message.role,
            truncate_chars(text.trim(), 500)
        );
        if summary.len().saturating_add(entry.len()) > LOCAL_COMPACTION_SUMMARY_MAX_CHARS {
            omitted += 1;
            continue;
        }
        summary.push_str(&entry);
    }
    if omitted > 0 {
        let _ = writeln!(
            summary,
            "- ({omitted} older messages omitted from local fallback summary to keep context bounded)"
        );
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
            messages::ContentBlock::Thinking { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(output, "thinking: {}", text.trim());
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

fn prepare_history_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create history directory {}", parent.display()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "failed to set permissions on history directory {}",
                parent.display()
            )
        })?;
    }
    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!(
                "failed to set permissions on history file {}",
                path.display()
            )
        })?;
    } else {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create history file {}", path.display()))?;
    }
    Ok(())
}

fn save_history_private(rl: &mut Editor<FerrumLineHelper, DefaultHistory>, path: &Path) {
    if let Err(error) = prepare_history_file(path).and_then(|()| {
        rl.save_history(path)
            .with_context(|| format!("failed to save history file {}", path.display()))
    }) {
        eprintln!("[history] {error}");
    }
    let _ = prepare_history_file(path);
}

fn sanitize_history_input(input: &str) -> String {
    input
        .split_whitespace()
        .map(|part| {
            let trimmed = part.trim_matches(['\'', '"']);
            if trimmed.starts_with("data:image/") {
                "[image omitted]".to_string()
            } else if is_ferrum_temp_image_path(Path::new(trimmed))
                || trimmed.contains("/ferrum-clipboard-")
            {
                "[clipboard image]".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn ferrum_temp_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("ferrum");
    fs::create_dir_all(&dir).with_context(|| {
        format!(
            "failed to create temporary image directory {}",
            dir.display()
        )
    })?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set permissions on temporary image directory {}",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn write_private_temp_file(prefix: &str, suffix: &str, data: &[u8]) -> Result<PathBuf> {
    let dir = ferrum_temp_dir()?;
    for _ in 0..16 {
        let path = dir.join(format!("{prefix}{}{suffix}", uuid::Uuid::new_v4()));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(data)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("failed to sync {}", path.display()))?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("failed to set permissions on {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create temporary image file in {}", dir.display())
                });
            }
        }
    }
    anyhow::bail!(
        "failed to create unique temporary image file in {}",
        dir.display()
    )
}

fn is_ferrum_temp_image_path(path: &Path) -> bool {
    ferrum_temp_dir().is_ok_and(|dir| path.starts_with(dir))
}

fn remove_if_ferrum_temp_image(path: &Path) {
    if is_ferrum_temp_image_path(path) {
        let _ = fs::remove_file(path);
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
    is_known_slash_command(first)
}

fn is_known_slash_command(command: &str) -> bool {
    command.starts_with("/skill:")
        || matches!(
            command,
            "/help"
                | "/version"
                | "/session"
                | "/title"
                | "/sessions"
                | "/skills"
                | "/skill"
                | "/model"
                | "/models"
                | "/provider"
                | "/providers"
                | "/thinking"
                | "/safety"
                | "/mcp"
                | "/colors"
                | "/palette"
                | "/palettes"
                | "/diff"
                | "/image"
                | "/image-paste"
                | "/paste-image"
                | "/usage"
                | "/compact"
                | "/quit"
                | "/exit"
        )
}

fn extract_pasted_images(input: &str, cwd: &Path) -> (String, Vec<String>) {
    let mut prompt_parts = Vec::new();
    let mut image_paths = Vec::new();

    for part in split_shell_like_words(input) {
        let trimmed = part.as_str();
        let path_candidate = ui_path_argument(trimmed, cwd);
        if trimmed.starts_with("data:image/")
            || looks_like_image_path(trimmed)
                && builtin_tools::path::resolve_to_cwd(&path_candidate, cwd)
                    .is_ok_and(|path| path.is_file())
        {
            image_paths.push(path_candidate);
        } else {
            prompt_parts.push(part);
        }
    }

    (prompt_parts.join(" "), image_paths)
}

fn split_shell_like_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (None, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn ui_path_argument(spec: &str, cwd: &Path) -> String {
    if let Some(stripped) = spec.strip_prefix('@')
        && builtin_tools::path::resolve_to_cwd(spec, cwd).is_ok_and(|path| !path.exists())
        && builtin_tools::path::resolve_to_cwd(stripped, cwd).is_ok_and(|path| path.exists())
    {
        return stripped.to_string();
    }
    spec.to_string()
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
        ..
    } = image
    else {
        anyhow::bail!("clipboard did not contain an image")
    };
    let bytes = STANDARD
        .decode(data_base64)
        .context("failed to decode clipboard image")?;
    write_private_temp_file(
        "ferrum-clipboard-",
        &format!(".{}", messages::image_extension(&mime_type)),
        &bytes,
    )
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
        if let Some(format) = chafa_pixel_format() {
            let args = vec![
                format!("--format={format}"),
                "--passthrough=auto".to_string(),
                format!("--size={}", chafa_preview_size(true)),
                "--scale=max".to_string(),
            ];
            if render_chafa_preview(path, &args) {
                if let Some(path) = temp_path {
                    let _ = fs::remove_file(path);
                }
                return;
            }
        }

        let args = vec![
            "--format=symbols".to_string(),
            format!("--size={}", chafa_preview_size(false)),
        ];
        if render_chafa_preview(path, &args) {
            if let Some(path) = temp_path {
                let _ = fs::remove_file(path);
            }
            return;
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

fn render_chafa_preview(path: &Path, args: &[String]) -> bool {
    Command::new("chafa")
        .args(args)
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

fn chafa_preview_size(pixel_graphics: bool) -> String {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let max_cols = if pixel_graphics { 120 } else { 80 };
    let max_rows = if pixel_graphics { 40 } else { 24 };
    let cols = cols.clamp(40, max_cols);
    let rows = rows.saturating_sub(6).clamp(12, max_rows);
    format!("{cols}x{rows}")
}

fn chafa_pixel_format() -> Option<&'static str> {
    chafa_pixel_format_for_env(
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        std::env::var_os("WEZTERM_EXECUTABLE").is_some(),
    )
}

fn chafa_pixel_format_for_env(
    term: Option<&str>,
    term_program: Option<&str>,
    has_kitty_window_id: bool,
    has_wezterm: bool,
) -> Option<&'static str> {
    let term = term.unwrap_or_default().to_ascii_lowercase();
    let term_program = term_program.unwrap_or_default().to_ascii_lowercase();

    if has_kitty_window_id
        || term.contains("kitty")
        || term.contains("ghostty")
        || term_program.contains("ghostty")
    {
        return Some("kitty");
    }

    if term_program.contains("iterm") {
        return Some("iterm");
    }

    if has_wezterm || term.contains("sixel") || term.contains("foot") || term.contains("mlterm") {
        return Some("sixels");
    }

    None
}

fn write_temp_image(image: &messages::ContentBlock) -> Result<PathBuf> {
    let messages::ContentBlock::Image {
        mime_type,
        data_base64,
        ..
    } = image
    else {
        anyhow::bail!("not an image")
    };
    let data = STANDARD
        .decode(data_base64)
        .context("failed to decode image for preview")?;
    write_private_temp_file(
        "ferrum-image-",
        &format!(".{}", messages::image_extension(mime_type)),
        &data,
    )
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
}

enum ContextTokenSource {
    UsagePlusEstimate,
    Estimate,
    EstimateAfterCompaction,
}

impl ContextTokenSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::UsagePlusEstimate => "usage+estimate",
            Self::Estimate => "estimate",
            Self::EstimateAfterCompaction => "estimate_after_compaction",
        }
    }
}

struct SessionStats {
    messages: usize,
    chars: usize,
    estimated_tokens: usize,
    context_source: ContextTokenSource,
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
    builtin_tools::shell_guard::validate(command, state.safety)?;
    let output = builtin_tools::bash::run(command, &state.cwd, Duration::from_secs(120)).await?;
    let rendered = render_bash_output(command, &output);

    if send_to_model {
        state.run_turn(rendered, config, true).await?;
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
            println!("  /title [text]         show or set session title");
            println!("  /sessions             list recent sessions for current directory");
            println!("  /sessions pick        open session picker");
            println!("  /sessions del         delete session via picker");
            println!("  /sessions new         start a new session");
            println!("  /skills               list available skills");
            println!("  /skill <name> [args]  load a skill into context");
            println!("  /skill:<name> [args]  load a skill into context");
            println!("  /model [name]         show or set model");
            println!("  /models               list known models for current provider");
            println!("  /usage [period]       show token usage: day|week|month");
            println!("  /provider [name]      show or set provider");
            println!("  /providers            list configured providers");
            println!("  /mcp [on|off|status|list] show or toggle MCP tools");
            println!("  /colors [mode]        show or set colors: auto|on|off");
            println!("  /palette [name]       show current palette or apply a palette");
            println!("  /palettes             list palettes from color-palettes/");
            println!(
                "  /thinking [level]     show or set thinking: off|minimal|low|medium|high|xhigh"
            );
            println!("  /safety [level]       show or set shell guard: low|medium|high");
            println!(
                "  /diff [mode]          show or set edit diff: unified|compact|full|words|side_by_side"
            );
            println!("  /image <path>         attach image to next message");
            println!("  /image-paste          attach image from clipboard");
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
            match parts.next() {
                Some(other) => {
                    anyhow::bail!("unknown /session subcommand: {other}");
                }
                None => {
                    println!("path: {}", state.session.path().display());
                    let stats = state.stats();
                    println!("messages: {}", stats.messages);
                    let info = session::jsonl::session_info(state.session.path())?
                        .ok_or_else(|| anyhow::anyhow!("current session metadata unavailable"))?;
                    println!("archived_messages: {}", info.archived_message_count);
                    println!("compactions: {}", info.compaction_count);
                    if let Some(timestamp) = info.last_compaction_timestamp_ms {
                        println!("last_compaction: {}", format_timestamp_ms(timestamp));
                    } else {
                        println!("last_compaction: none");
                    }
                    println!("chars: {}", stats.chars);
                    println!("context_tokens: {}", stats.estimated_tokens);
                    println!("context_source: {}", stats.context_source.as_str());
                    println!("max_context_tokens: {}", config.max_context_tokens);
                    println!(
                        "context_usage_percent: {}",
                        context_usage_percent(stats.estimated_tokens, config.max_context_tokens)
                    );
                    println!("max_tool_rounds: {}", config.max_tool_rounds);
                    println!("file_bytes: {}", stats.file_bytes);
                    println!("pending_images: {}", state.pending_images.len());
                    println!("skills: {}", state.skills.len());
                    println!("mcp_enabled: {}", state.mcp_enabled);
                    println!("mcp_connected: {}", state.mcp.is_some());
                    println!("diff_mode: {}", state.diff_mode.as_str());
                    println!("safety: {}", state.safety.as_str());
                    println!("model: {}", config.model);
                    if config.provider_model != config.model {
                        println!("provider_model: {}", config.provider_model);
                    }
                    println!("thinking: {}", config.thinking.as_str());
                    println!("provider: {}", config.provider_name);
                }
            }
            Ok(CommandAction::Continue)
        }
        "/title" => {
            let title = parts.collect::<Vec<_>>().join(" ");
            if title.trim().is_empty() {
                let info = session::jsonl::session_info(state.session.path())?
                    .ok_or_else(|| anyhow::anyhow!("current session metadata unavailable"))?;
                println!("title: {}", info.title);
            } else {
                state.set_title(&title)?;
                println!("title: {}", title.trim());
            }
            Ok(CommandAction::Continue)
        }
        "/sessions" => {
            match parts.next() {
                None => state.list_sessions(config)?,
                Some("pick") => state.pick_session(config)?,
                Some("del") => state.delete_session_picker(config)?,
                Some("new") => state.new_session(config)?,
                Some(reference) if reference.chars().all(|ch| ch.is_ascii_digit()) => {
                    anyhow::bail!(
                        "numeric session shortcuts were removed; use /sessions pick or /sessions del"
                    )
                }
                Some(reference) => {
                    anyhow::bail!(
                        "unknown /sessions subcommand: {reference}. Use /sessions, /sessions pick, /sessions del, or /sessions new"
                    )
                }
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
                config.set_model(model)?;
                state.session.append_provider(&config.provider_name)?;
                state.session.append_model(&config.model)?;
                state.refresh_runtime_context(config)?;
            }
            println!("model: {}", config.model);
            if config.provider_model != config.model {
                println!("provider_model: {}", config.provider_model);
            }
            Ok(CommandAction::Continue)
        }
        "/models" => {
            anyhow::bail!("/models is async; this command should be handled before sync commands")
        }
        "/usage" => {
            let period = usage::UsagePeriod::parse(parts.next())?;
            if let Some(extra) = parts.next() {
                anyhow::bail!("usage: /usage [day|week|month], got extra argument: {extra}");
            }
            let rows = usage::summarize_usage(&config.data_dir, period, usage::now_unix())?;
            print_usage_summary(period, &rows);
            Ok(CommandAction::Continue)
        }
        "/provider" => {
            if let Some(provider) = parts.next() {
                config.set_provider(provider)?;
                state.session.append_provider(&config.provider_name)?;
                state.session.append_model(&config.model)?;
                state.refresh_runtime_context(config)?;
            }
            println!("provider: {}", config.provider_name);
            println!("model: {}", config.model);
            if config.provider_model != config.model {
                println!("provider_model: {}", config.provider_model);
            }
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
                    state.refresh_runtime_context(config)?;
                }
                Some("off") => {
                    config.mcp_enabled = false;
                    state.set_mcp_enabled(false)?;
                    state.refresh_runtime_context(config)?;
                }
                Some(other) => anyhow::bail!("usage: /mcp [on|off|status|list], got: {other}"),
            }
            Ok(CommandAction::Continue)
        }
        "/colors" => {
            if let Some(mode) = parts.next() {
                let parsed = ColorMode::parse(mode)?;
                config.color_mode = parsed;
                state.color_mode = parsed;
                state.session.append_color_mode(parsed.as_str())?;
            }
            println!("colors: {}", config.color_mode.as_str());
            Ok(CommandAction::Continue)
        }
        "/palette" => {
            match parts.next() {
                None => println!(
                    "palette: {}",
                    current_palette_name(&config.config_dir, &state.colors)?
                ),
                Some(name) => {
                    if let Some(extra) = parts.next() {
                        anyhow::bail!("usage: /palette [name], got extra argument: {extra}");
                    }
                    apply_palette(name, config, state)?;
                }
            }
            Ok(CommandAction::Continue)
        }
        "/palettes" => {
            if let Some(extra) = parts.next() {
                anyhow::bail!("usage: /palettes, got extra argument: {extra}");
            }
            print_palette_list(&config.config_dir)?;
            Ok(CommandAction::Continue)
        }
        "/thinking" => {
            if let Some(thinking) = parts.next() {
                config.thinking = crate::config::ThinkingLevel::parse(thinking)?;
                state.session.append_thinking(config.thinking.as_str())?;
                state.refresh_runtime_context(config)?;
            }
            println!("thinking: {}", config.thinking.as_str());
            Ok(CommandAction::Continue)
        }
        "/safety" => {
            if let Some(level) = parts.next() {
                let parsed = SafetyLevel::parse(level)?;
                config.safety = parsed;
                state.safety = parsed;
                state.session.append_safety(parsed.as_str())?;
                state.refresh_runtime_context(config)?;
            }
            println!("safety: {}", state.safety.as_str());
            match state.safety {
                SafetyLevel::Low => println!(
                    "shell guard: permissive. Allows common shell syntax; blocks destructive commands and obvious obfuscation."
                ),
                SafetyLevel::Medium => println!(
                    "shell guard: balanced. Allows normal coding commands; blocks destructive commands and ambiguous shell tricks like command substitution."
                ),
                SafetyLevel::High => println!(
                    "shell guard: strict. Allows simple inspection/build commands; also blocks common direct network-capable commands, inline interpreters, direct scripts, and broad disk writes."
                ),
            }
            Ok(CommandAction::Continue)
        }
        "/diff" => {
            if let Some(mode) = parts.next() {
                let parsed = DiffMode::parse(mode)?;
                config.diff_mode = parsed;
                state.diff_mode = parsed;
                state.session.append_diff_mode(parsed.as_str())?;
                state.refresh_runtime_context(config)?;
            }
            println!("diff: {}", state.diff_mode.as_str());
            Ok(CommandAction::Continue)
        }
        "/image" => {
            let raw_args = input[command.len()..].trim();
            let args = split_shell_like_words(raw_args);
            if args.len() != 1 {
                anyhow::bail!("usage: /image <path>");
            }
            let path = &args[0];
            state.attach_images(vec![path.to_string()])?;
            println!("attached image: {path}");
            Ok(CommandAction::Continue)
        }
        "/image-paste" | "/paste-image" => {
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
