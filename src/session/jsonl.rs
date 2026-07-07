use crate::agent::messages::{Message, Role};
use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug)]
pub struct JsonlSession {
    path: PathBuf,
    file: File,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Header {
        id: String,
        parent_id: Option<String>,
        timestamp_ms: u64,
        version: u32,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
        cwd: Option<String>,
    },
    Message {
        id: String,
        parent_id: Option<String>,
        timestamp_ms: u64,
        message: Message,
    },
    Compaction {
        id: String,
        parent_id: Option<String>,
        timestamp_ms: u64,
        summary: String,
    },
    Metadata {
        id: String,
        parent_id: Option<String>,
        timestamp_ms: u64,
        title: Option<String>,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        color_mode: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    },
}

impl JsonlSession {
    pub fn create(
        dir: PathBuf,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::create_with_header_id(
            dir,
            format!("{}.jsonl", now_ms()),
            Uuid::new_v4().to_string(),
            provider,
            model,
            thinking,
            diff_mode,
            safety,
            tools,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_named(
        dir: PathBuf,
        id: &str,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        validate_user_session_id(id)?;
        Self::create_with_header_id(
            dir,
            format!("{id}.jsonl"),
            id.to_string(),
            provider,
            model,
            thinking,
            diff_mode,
            safety,
            tools,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_with_header_id(
        dir: PathBuf,
        filename: String,
        header_id: String,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        tighten_dir_permissions(&dir);
        let path = dir.join(filename);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut session = Self { path, file };
        session.append(&SessionEntry::Header {
            id: header_id,
            parent_id: None,
            timestamp_ms: now_ms(),
            version: 1,
            provider,
            model,
            thinking,
            diff_mode,
            safety,
            tools,
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.display().to_string()),
        })?;
        Ok(session)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        tighten_file_permissions(&path);
        let file = OpenOptions::new()
            .append(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        Ok(Self { path, file })
    }

    pub fn append_message(&mut self, message: &Message) -> Result<()> {
        self.append(&SessionEntry::Message {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            message: message.clone(),
        })
    }

    pub fn append_compaction(&mut self, summary: &str) -> Result<()> {
        self.append(&SessionEntry::Compaction {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            summary: summary.to_string(),
        })
    }

    pub fn append_title(&mut self, title: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: Some(title.to_string()),
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn append_thinking(&mut self, thinking: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: None,
            thinking: Some(thinking.to_string()),
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn append_diff_mode(&mut self, diff_mode: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: Some(diff_mode.to_string()),
            safety: None,
            tools: None,
        })
    }

    pub fn append_safety(&mut self, safety: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: Some(safety.to_string()),
            tools: None,
        })
    }

    pub fn append_tools(&mut self, tools: &[String]) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: Some(tools.to_vec()),
        })
    }

    pub fn append_color_mode(&mut self, color_mode: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: None,
            thinking: None,
            color_mode: Some(color_mode.to_string()),
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn append_provider(&mut self, provider: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: Some(provider.to_string()),
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn append_model(&mut self, model: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: None,
            model: Some(model.to_string()),
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn remove_if_empty(&mut self) -> Result<bool> {
        self.file.flush().context("failed to flush session")?;
        if !session_has_entries_after_header(&self.path)? {
            fs::remove_file(&self.path).with_context(|| {
                format!("failed to remove empty session {}", self.path.display())
            })?;
            return Ok(true);
        }
        Ok(false)
    }

    fn append(&mut self, entry: &SessionEntry) -> Result<()> {
        serde_json::to_writer(&mut self.file, entry)
            .context("failed to serialize session entry")?;
        self.file
            .write_all(b"\n")
            .context("failed to write session newline")?;
        self.file.flush().context("failed to flush session")?;
        Ok(())
    }
}

fn session_has_entries_after_header(path: &Path) -> Result<bool> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if matches!(
            entry,
            SessionEntry::Message { .. } | SessionEntry::Compaction { .. }
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn load_messages(path: &Path) -> Result<Vec<Message>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        match entry {
            SessionEntry::Message { message, .. } => messages.push(message),
            SessionEntry::Compaction { summary, .. } => {
                messages.clear();
                messages.push(Message::text(
                    Role::System,
                    format!("Conversation summary from previous compaction:\n{summary}"),
                ));
            }
            SessionEntry::Header { .. } | SessionEntry::Metadata { .. } => {}
        }
    }
    Ok(messages)
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub short_id: String,
    pub path: PathBuf,
    pub cwd: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub color_mode: Option<String>,
    pub diff_mode: Option<String>,
    pub safety: Option<String>,
    pub tools: Option<Vec<String>>,
    pub title: String,
    pub message_count: usize,
    pub archived_message_count: usize,
    pub compaction_count: usize,
    pub last_compaction_timestamp_ms: Option<u64>,
    pub modified: SystemTime,
}

pub fn latest_session_for_cwd(dir: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    Ok(list_sessions_for_cwd(dir, cwd)?
        .first()
        .map(|info| info.path.clone()))
}

pub fn list_sessions_for_cwd(dir: &Path, cwd: &Path) -> Result<Vec<SessionInfo>> {
    let cwd = cwd.display().to_string();
    let mut sessions = list_sessions(dir)?
        .into_iter()
        .filter(|session| session.cwd.as_deref() == Some(cwd.as_str()))
        .collect::<Vec<_>>();
    sort_sessions_newest_first(&mut sessions);
    Ok(sessions)
}

pub fn list_sessions(dir: &Path) -> Result<Vec<SessionInfo>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        if let Some(info) = session_info(&path)? {
            sessions.push(info);
        }
    }
    sort_sessions_newest_first(&mut sessions);
    Ok(sessions)
}

pub fn resolve_session_ref(dir: &Path, cwd: &Path, reference: &str) -> Result<PathBuf> {
    let path = PathBuf::from(reference);
    if reference.contains('/') || reference.ends_with(".jsonl") {
        return Ok(path);
    }
    let matches = list_sessions_for_cwd(dir, cwd)?
        .into_iter()
        .filter(|session| {
            session.id.starts_with(reference) || session.short_id.starts_with(reference)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session.path.clone()),
        [] => anyhow::bail!("no session matches '{reference}' in current directory"),
        _ => anyhow::bail!("session reference '{reference}' is ambiguous"),
    }
}

#[derive(Debug, Clone)]
pub struct HistorySearchOptions {
    pub query: String,
    pub literal: bool,
    pub ignore_case: bool,
    pub limit: usize,
}

const HISTORY_SNIPPET_CHARS: usize = 240;

pub fn search_history(path: &Path, options: HistorySearchOptions) -> Result<String> {
    let matcher = HistoryMatcher::new(&options.query, options.literal, options.ignore_case)?;
    let limit = options.limit.clamp(1, 50);
    let entries = parsed_session_entries(path)?;
    let archive_cutoff = latest_compaction_line(&entries).unwrap_or(0);
    let mut out = String::new();
    let mut count = 0usize;

    for entry in entries {
        let Some((role, text)) = searchable_entry_text(&entry.entry) else {
            continue;
        };
        let Some((start, end)) = matcher.find(&text) else {
            continue;
        };
        let status = if entry.line_number < archive_cutoff {
            "archived"
        } else {
            "active"
        };
        let snippet = history_snippet(&text, start, end, HISTORY_SNIPPET_CHARS);
        out.push_str(&format!(
            "line {} {status} {role}: {snippet}\n",
            entry.line_number
        ));
        count += 1;
        if count >= limit {
            break;
        }
    }

    if out.is_empty() {
        out.push_str("no history matches\n");
    }
    Ok(out)
}

pub fn read_history(path: &Path, offset: usize, limit: usize) -> Result<String> {
    let offset = offset.max(1);
    let limit = limit.clamp(1, 100);
    let entries = parsed_session_entries(path)?;
    let archive_cutoff = latest_compaction_line(&entries).unwrap_or(0);
    let mut out = String::new();
    let mut count = 0usize;

    for entry in entries
        .into_iter()
        .filter(|entry| entry.line_number >= offset)
    {
        let status = if entry.line_number < archive_cutoff {
            "archived"
        } else {
            "active"
        };
        match render_history_entry(&entry.entry) {
            Some((kind, text)) => {
                out.push_str(&format!("line {} {status} {kind}:\n", entry.line_number));
                for line in text.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            None => out.push_str(&format!("line {} {status}: metadata\n", entry.line_number)),
        }
        count += 1;
        if count >= limit {
            break;
        }
    }

    if out.is_empty() {
        out.push_str("no history lines\n");
    }
    Ok(out)
}

struct ParsedSessionEntry {
    line_number: usize,
    entry: SessionEntry,
}

fn parsed_session_entries(path: &Path) -> Result<Vec<ParsedSessionEntry>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line_number = index + 1;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        entries.push(ParsedSessionEntry { line_number, entry });
    }
    Ok(entries)
}

fn latest_compaction_line(entries: &[ParsedSessionEntry]) -> Option<usize> {
    entries
        .iter()
        .filter(|entry| matches!(entry.entry, SessionEntry::Compaction { .. }))
        .map(|entry| entry.line_number)
        .next_back()
}

fn searchable_entry_text(entry: &SessionEntry) -> Option<(&'static str, String)> {
    match entry {
        SessionEntry::Message { message, .. } => {
            let text = message_search_text(message);
            (!text.trim().is_empty()).then_some((message_role_name(message), text))
        }
        SessionEntry::Compaction { summary, .. } => Some(("compaction", summary.clone())),
        SessionEntry::Header { .. } | SessionEntry::Metadata { .. } => None,
    }
}

fn render_history_entry(entry: &SessionEntry) -> Option<(&'static str, String)> {
    match entry {
        SessionEntry::Message { message, .. } => {
            Some((message_role_name(message), message_history_text(message)))
        }
        SessionEntry::Compaction { summary, .. } => Some(("compaction", summary.clone())),
        SessionEntry::Header { .. } | SessionEntry::Metadata { .. } => None,
    }
}

fn message_role_name(message: &Message) -> &'static str {
    match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn message_search_text(message: &Message) -> String {
    message
        .content
        .iter()
        .map(|block| match block {
            crate::agent::messages::ContentBlock::Text { text } => text.clone(),
            crate::agent::messages::ContentBlock::Thinking { text, .. } => {
                format!("thinking: {text}")
            }
            crate::agent::messages::ContentBlock::ToolResult {
                content, is_error, ..
            } => format!("tool_result error={is_error}: {content}"),
            crate::agent::messages::ContentBlock::Image { source, sha256, .. } => {
                format!("image {source} sha256={sha256}")
            }
            crate::agent::messages::ContentBlock::ToolUse { name, input, .. } => {
                format!("tool_call {name}: {input}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_history_text(message: &Message) -> String {
    message
        .content
        .iter()
        .map(|block| match block {
            crate::agent::messages::ContentBlock::Text { text } => text.clone(),
            crate::agent::messages::ContentBlock::Thinking { text, .. } => {
                format!("thinking: {text}")
            }
            crate::agent::messages::ContentBlock::ToolUse { name, input, .. } => {
                format!("tool_call {name}: {input}")
            }
            crate::agent::messages::ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                format!("tool_result error={is_error}: {content}")
            }
            crate::agent::messages::ContentBlock::Image { source, sha256, .. } => {
                format!("image {source} sha256={sha256}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct HistoryMatcher(Regex);

impl HistoryMatcher {
    fn new(query: &str, literal: bool, ignore_case: bool) -> Result<Self> {
        let pattern = if literal {
            regex::escape(query)
        } else {
            query.to_string()
        };
        Ok(Self(
            RegexBuilder::new(&pattern)
                .case_insensitive(ignore_case)
                .build()
                .with_context(|| format!("invalid history search pattern: {query}"))?,
        ))
    }

    fn find(&self, text: &str) -> Option<(usize, usize)> {
        self.0.find(text).map(|found| (found.start(), found.end()))
    }
}

fn history_snippet(text: &str, start: usize, end: usize, max_chars: usize) -> String {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    let before = max_chars / 3;
    let after = max_chars.saturating_sub(before);
    let snippet_start = text[..start]
        .char_indices()
        .rev()
        .nth(before)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let snippet_end = text[end..]
        .char_indices()
        .nth(after)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    let mut snippet = String::new();
    if snippet_start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(
        &text[snippet_start..snippet_end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    );
    if snippet_end < text.len() {
        snippet.push_str("...");
    }
    snippet
}

pub enum SessionRefResolution {
    Existing(PathBuf),
    Created(PathBuf),
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_or_create_session_ref(
    dir: &Path,
    cwd: &Path,
    reference: &str,
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    diff_mode: Option<String>,
    safety: Option<String>,
    tools: Option<Vec<String>>,
) -> Result<SessionRefResolution> {
    match resolve_session_ref(dir, cwd, reference) {
        Ok(path) => Ok(SessionRefResolution::Existing(path)),
        Err(_) if is_valid_user_session_id(reference) => {
            let path = dir.join(format!("{reference}.jsonl"));
            if path.exists() {
                return Ok(SessionRefResolution::Existing(path));
            }
            JsonlSession::create_named(
                dir.to_path_buf(),
                reference,
                provider,
                model,
                thinking,
                diff_mode,
                safety,
                tools,
            )?;
            Ok(SessionRefResolution::Created(path))
        }
        Err(error) => Err(error),
    }
}

pub fn session_info(path: &Path) -> Result<Option<SessionInfo>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH);
    let mut id = None;
    let mut cwd = None;
    let mut provider = None;
    let mut model = None;
    let mut inferred_title = None;
    let mut explicit_title = None;
    let mut explicit_provider = None;
    let mut explicit_model = None;
    let mut explicit_thinking = None;
    let mut explicit_color_mode = None;
    let mut explicit_diff_mode = None;
    let mut explicit_safety = None;
    let mut explicit_tools = None;
    let mut total_message_count = 0usize;
    let mut visible_message_count = 0usize;
    let mut archived_message_count = 0usize;
    let mut compaction_count = 0usize;
    let mut last_compaction_timestamp_ms = None;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        match entry {
            SessionEntry::Header {
                id: header_id,
                provider: header_provider,
                model: header_model,
                thinking: header_thinking,
                diff_mode: header_diff_mode,
                safety: header_safety,
                tools: header_tools,
                cwd: header_cwd,
                ..
            } => {
                id = Some(header_id);
                explicit_provider = header_provider.clone();
                explicit_model = header_model.clone();
                provider = header_provider;
                model = header_model;
                explicit_thinking = header_thinking;
                explicit_diff_mode = header_diff_mode;
                explicit_safety = header_safety;
                explicit_tools = header_tools;
                cwd = header_cwd;
            }
            SessionEntry::Message { message, .. } => {
                total_message_count += 1;
                visible_message_count += 1;
                if inferred_title.is_none() && matches!(message.role, Role::User) {
                    let text = message.text_content();
                    if !text.trim().is_empty() {
                        inferred_title = Some(one_line_title(&text));
                    }
                }
            }
            SessionEntry::Metadata {
                title,
                provider,
                model,
                thinking,
                color_mode,
                diff_mode,
                safety,
                tools,
                ..
            } => {
                if let Some(title) = title
                    && !title.trim().is_empty()
                {
                    explicit_title = Some(one_line_title(&title));
                }
                if let Some(provider) = provider
                    && !provider.trim().is_empty()
                {
                    explicit_provider = Some(provider);
                }
                if let Some(model) = model
                    && !model.trim().is_empty()
                {
                    explicit_model = Some(model);
                }
                if let Some(thinking) = thinking
                    && !thinking.trim().is_empty()
                {
                    explicit_thinking = Some(thinking);
                }
                if let Some(color_mode) = color_mode
                    && !color_mode.trim().is_empty()
                {
                    explicit_color_mode = Some(color_mode);
                }
                if let Some(diff_mode) = diff_mode
                    && !diff_mode.trim().is_empty()
                {
                    explicit_diff_mode = Some(diff_mode);
                }
                if let Some(safety) = safety
                    && !safety.trim().is_empty()
                {
                    explicit_safety = Some(safety);
                }
                if let Some(tools) = tools {
                    explicit_tools = Some(tools);
                }
            }
            SessionEntry::Compaction { timestamp_ms, .. } => {
                archived_message_count = total_message_count;
                visible_message_count = 1;
                compaction_count += 1;
                last_compaction_timestamp_ms = Some(timestamp_ms);
            }
        }
    }

    let Some(id) = id else {
        return Ok(None);
    };
    let short_id = id.chars().take(8).collect();
    Ok(Some(SessionInfo {
        id,
        short_id,
        path: path.to_path_buf(),
        cwd,
        provider: explicit_provider.or(provider),
        model: explicit_model.or(model),
        thinking: explicit_thinking,
        color_mode: explicit_color_mode,
        diff_mode: explicit_diff_mode,
        safety: explicit_safety,
        tools: explicit_tools,
        title: explicit_title
            .or(inferred_title)
            .unwrap_or_else(|| "(empty session)".to_string()),
        message_count: visible_message_count,
        archived_message_count,
        compaction_count,
        last_compaction_timestamp_ms,
        modified,
    }))
}

fn validate_user_session_id(id: &str) -> Result<()> {
    if is_valid_user_session_id(id) {
        return Ok(());
    }
    anyhow::bail!(
        "invalid session id '{id}'; use 1-80 characters from A-Z, a-z, 0-9, '.', '_', or '-', and do not start with '.'"
    )
}

fn is_valid_user_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && !id.starts_with('.')
        && id != ".."
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn sort_sessions_newest_first(sessions: &mut [SessionInfo]) {
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
}

fn one_line_title(text: &str) -> String {
    let title = text
        .split_whitespace()
        .take(18)
        .collect::<Vec<_>>()
        .join(" ");
    if title.chars().count() > 120 {
        title.chars().take(120).collect()
    } else {
        title
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn tighten_dir_permissions(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o077 != 0 {
            permissions.set_mode(0o700);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

fn tighten_file_permissions(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o177 != 0 {
            permissions.set_mode(0o600);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::messages::{Message, Role};

    #[test]
    fn session_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let mode = fs::metadata(session.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn creates_named_session_with_user_id() {
        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create_named(
            temp.path().to_path_buf(),
            "mysession",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(session.path(), &temp.path().join("mysession.jsonl"));
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.id, "mysession");
    }

    #[test]
    fn rejects_unsafe_named_session_ids() {
        let temp = tempfile::tempdir().unwrap();
        for id in ["", ".hidden", "..", "bad/name", "bad name"] {
            assert!(
                JsonlSession::create_named(
                    temp.path().to_path_buf(),
                    id,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn resolves_or_creates_named_session() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let path = resolve_or_create_session_ref(
            temp.path(),
            &cwd,
            "named-session",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        match path {
            SessionRefResolution::Created(path) => {
                assert_eq!(path, temp.path().join("named-session.jsonl"));
                assert!(path.exists());
            }
            SessionRefResolution::Existing(_) => panic!("expected created named session"),
        }

        let again = resolve_or_create_session_ref(
            temp.path(),
            &cwd,
            "named-session",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        match again {
            SessionRefResolution::Existing(path) => {
                assert_eq!(path, temp.path().join("named-session.jsonl"));
            }
            SessionRefResolution::Created(_) => panic!("expected existing named session"),
        }
    }

    #[test]
    fn writes_header_and_message_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "hello"))
            .unwrap();

        let text = fs::read_to_string(session.path()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"type\":\"header\""));
        assert!(lines[0].contains("\"cwd\""));
        assert!(lines[1].contains("\"type\":\"message\""));
    }

    #[test]
    fn compaction_replaces_prior_messages_when_loading() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "old context"))
            .unwrap();
        session.append_compaction("summary checkpoint").unwrap();
        session
            .append_message(&Message::text(Role::User, "new context"))
            .unwrap();

        let messages = load_messages(session.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert!(messages[0].text_content().contains("summary checkpoint"));
        assert_eq!(messages[1].text_content(), "new context");
        assert!(
            !messages
                .iter()
                .any(|message| message.text_content() == "old context")
        );
    }

    #[test]
    fn latest_compaction_replaces_earlier_compaction_when_loading() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "old context"))
            .unwrap();
        session.append_compaction("first summary").unwrap();
        session
            .append_message(&Message::text(Role::User, "middle context"))
            .unwrap();
        session.append_compaction("second summary").unwrap();
        session
            .append_message(&Message::text(Role::User, "new context"))
            .unwrap();

        let messages = load_messages(session.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(messages[0].text_content().contains("second summary"));
        assert!(!messages[0].text_content().contains("first summary"));
        assert_eq!(messages[1].text_content(), "new context");
    }

    #[test]
    fn session_info_counts_visible_messages_after_compaction() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "old context"))
            .unwrap();
        session.append_compaction("summary checkpoint").unwrap();
        session
            .append_message(&Message::text(Role::User, "new context"))
            .unwrap();

        let info = session_info(session.path()).unwrap().unwrap();

        assert_eq!(info.message_count, 2);
        assert_eq!(info.archived_message_count, 1);
        assert_eq!(info.compaction_count, 1);
        assert!(info.last_compaction_timestamp_ms.is_some());
    }

    fn message_text(message: &Message) -> &str {
        match message.content.first().unwrap() {
            crate::agent::messages::ContentBlock::Text { text } => text,
            _ => panic!("expected text message"),
        }
    }

    #[test]
    fn load_messages_skips_malformed_jsonl_lines() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "before"))
            .unwrap();
        use std::io::Write;
        writeln!(session.file, "{{not json").unwrap();
        session
            .append_message(&Message::text(Role::Assistant, "after"))
            .unwrap();

        let messages = load_messages(session.path()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(message_text(&messages[0]), "before");
        assert_eq!(message_text(&messages[1]), "after");
    }

    #[test]
    fn history_search_and_read_render_archived_tool_context() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "debug slurm job 42"))
            .unwrap();
        session
            .append_message(&Message {
                role: Role::Tool,
                content: vec![crate::agent::messages::ContentBlock::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "sacct says OUT_OF_MEMORY on node n004".to_string(),
                    is_error: false,
                }],
                usage: None,
            })
            .unwrap();
        session.append_compaction("summary checkpoint").unwrap();
        session
            .append_message(&Message::text(Role::Assistant, "raise --mem and retry"))
            .unwrap();

        let matches = search_history(
            session.path(),
            HistorySearchOptions {
                query: "out_of_memory".to_string(),
                literal: true,
                ignore_case: true,
                limit: 10,
            },
        )
        .unwrap();
        assert!(matches.contains("archived tool"));
        assert!(matches.contains("OUT_OF_MEMORY"));

        let read = read_history(session.path(), 3, 2).unwrap();
        assert!(read.contains("line 3 archived tool"));
        assert!(read.contains("tool_result error=false"));
        assert!(read.contains("line 4 active compaction"));
    }

    #[test]
    fn removes_empty_header_only_session() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let path = session.path().clone();
        assert!(path.exists());
        assert!(session.remove_if_empty().unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn keeps_session_with_message() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "hello"))
            .unwrap();
        let path = session.path().clone();
        assert!(!session.remove_if_empty().unwrap());
        assert!(path.exists());
    }

    #[test]
    fn explicit_title_overrides_inferred_title() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session
            .append_message(&Message::text(Role::User, "inferred title"))
            .unwrap();
        session.append_title("explicit title").unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.title, "explicit title");
    }

    #[test]
    fn latest_explicit_title_wins() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        session.append_title("first title").unwrap();
        session.append_title("second title").unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.title, "second title");
    }

    #[test]
    fn stores_initial_thinking_in_header() {
        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            Some("medium".to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.thinking.as_deref(), Some("medium"));
    }

    #[test]
    fn latest_thinking_metadata_wins() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            Some("low".to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        session.append_thinking("high").unwrap();
        session.append_thinking("off").unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.thinking.as_deref(), Some("off"));
    }

    #[test]
    fn stores_initial_diff_mode_in_header() {
        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            Some("words".to_string()),
            None,
            None,
        )
        .unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.diff_mode.as_deref(), Some("words"));
    }

    #[test]
    fn latest_diff_mode_metadata_wins() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            Some("unified".to_string()),
            None,
            None,
        )
        .unwrap();
        session.append_diff_mode("full").unwrap();
        session.append_diff_mode("side_by_side").unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.diff_mode.as_deref(), Some("side_by_side"));
    }

    #[test]
    fn stores_initial_safety_in_header() {
        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            Some("high".to_string()),
            None,
        )
        .unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.safety.as_deref(), Some("high"));
    }

    #[test]
    fn latest_safety_metadata_wins() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            Some("medium".to_string()),
            None,
        )
        .unwrap();
        session.append_safety("low").unwrap();
        session.append_safety("high").unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.safety.as_deref(), Some("high"));
    }

    #[test]
    fn latest_tools_metadata_wins() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            Some(vec!["read".to_string()]),
        )
        .unwrap();
        session
            .append_tools(&["grep".to_string(), "find".to_string()])
            .unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(
            info.tools,
            Some(vec!["grep".to_string(), "find".to_string()])
        );
    }
}
