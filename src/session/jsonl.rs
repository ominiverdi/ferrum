use crate::{
    agent::messages::{Message, Role},
    persistence::{
        BoundedLine, MAX_JSONL_RECORD_BYTES, ensure_record_size, lock_exclusive, lock_shared,
        read_bounded_line, repair_incomplete_tail,
    },
};
use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Write},
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
        #[serde(default)]
        color_mode: Option<String>,
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
    #[allow(dead_code)]
    pub fn create(
        dir: PathBuf,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::create_with_color_mode(
            dir, provider, model, thinking, None, diff_mode, safety, tools,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_with_color_mode(
        dir: PathBuf,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        color_mode: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        for _ in 0..16 {
            let filename = format!("{}-{}.jsonl", now_ms(), Uuid::new_v4());
            match Self::create_with_header_id(
                dir.clone(),
                filename,
                Uuid::new_v4().to_string(),
                provider.clone(),
                model.clone(),
                thinking.clone(),
                color_mode.clone(),
                diff_mode.clone(),
                safety.clone(),
                tools.clone(),
            ) {
                Ok(session) => return Ok(session),
                Err(error)
                    if error.chain().any(|cause| {
                        cause
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
                    }) => {}
                Err(error) => return Err(error),
            }
        }
        anyhow::bail!("failed to allocate a unique anonymous session filename")
    }

    #[allow(dead_code)]
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
        Self::create_named_with_color_mode(
            dir, id, provider, model, thinking, None, diff_mode, safety, tools,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_named_with_color_mode(
        dir: PathBuf,
        id: &str,
        provider: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        color_mode: Option<String>,
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
            color_mode,
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
        color_mode: Option<String>,
        diff_mode: Option<String>,
        safety: Option<String>,
        tools: Option<Vec<String>>,
    ) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        tighten_dir_permissions(&dir);
        let path = dir.join(filename);
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut session = Self { path, file };
        let header = SessionEntry::Header {
            id: header_id,
            parent_id: None,
            timestamp_ms: now_ms(),
            version: 1,
            provider,
            model,
            thinking,
            color_mode,
            diff_mode,
            safety,
            tools,
            cwd: std::env::current_dir()
                .ok()
                .map(|path| canonical_display_path(&path)),
        };
        if let Err(error) = session.append(&header) {
            drop(session.file);
            let _ = fs::remove_file(&session.path);
            return Err(error);
        }
        File::open(&dir)
            .with_context(|| format!("failed to open {}", dir.display()))?
            .sync_all()
            .with_context(|| format!("failed to sync {}", dir.display()))?;
        Ok(session)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        validate_session_file(&path)?;
        tighten_file_permissions(&path);
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        {
            let _lock = lock_exclusive(&file)?;
            if repair_incomplete_tail(&mut file)? {
                file.sync_data()
                    .context("failed to sync repaired session")?;
            }
        }
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

    pub fn append_provider_model_transition(&mut self, provider: &str, model: &str) -> Result<()> {
        self.append(&SessionEntry::Metadata {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            title: None,
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
        })
    }

    pub fn sync_checkpoint(&mut self) -> Result<()> {
        self.file.flush().context("failed to flush session")?;
        self.file.sync_data().context("failed to sync session")
    }

    fn append(&mut self, entry: &SessionEntry) -> Result<()> {
        self.append_entries(std::slice::from_ref(entry))
    }

    fn append_entries(&mut self, entries: &[SessionEntry]) -> Result<()> {
        let mut records = Vec::new();
        for entry in entries {
            let record = serde_json::to_vec(entry).context("failed to serialize session entry")?;
            ensure_record_size(&record, "session")?;
            records.extend_from_slice(&record);
            records.push(b'\n');
        }
        let _lock = lock_exclusive(&self.file)?;
        repair_incomplete_tail(&mut self.file)?;
        self.file
            .write_all(&records)
            .context("failed to write session entry")?;
        self.file.flush().context("failed to flush session")?;
        self.file.sync_data().context("failed to sync session")?;
        Ok(())
    }
}

fn for_each_session_entry(
    path: &Path,
    visit: impl FnMut(usize, SessionEntry) -> Result<bool>,
) -> Result<()> {
    for_each_session_entry_with_diagnostics(path, true, visit)
}

fn for_each_session_entry_with_diagnostics(
    path: &Path,
    diagnose: bool,
    mut visit: impl FnMut(usize, SessionEntry) -> Result<bool>,
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let _lock = lock_shared(&file)?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    let mut line_number = 0usize;
    let mut diagnostics = 0usize;
    loop {
        let line = read_bounded_line(&mut reader, &mut bytes, MAX_JSONL_RECORD_BYTES)?;
        if line == BoundedLine::Eof {
            break;
        }
        line_number += 1;
        let BoundedLine::Line { terminated } = line else {
            if diagnose && diagnostics < 10 {
                eprintln!(
                    "[session] skipped oversized JSONL record at line {} in {}",
                    line_number,
                    path.display()
                );
            }
            diagnostics += 1;
            continue;
        };
        if !terminated {
            if diagnose && diagnostics < 10 {
                eprintln!(
                    "[session] skipped incomplete trailing JSONL record at line {} in {}",
                    line_number,
                    path.display()
                );
            }
            diagnostics += 1;
            break;
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(error) => {
                if diagnose && diagnostics < 10 {
                    eprintln!(
                        "[session] skipped malformed JSONL line {} in {}: {error}",
                        line_number,
                        path.display()
                    );
                }
                diagnostics += 1;
                continue;
            }
        };
        if !visit(line_number, entry)? {
            break;
        }
    }
    if diagnose && diagnostics > 10 {
        eprintln!(
            "[session] omitted {} additional corruption diagnostics for {}",
            diagnostics - 10,
            path.display()
        );
    }
    Ok(())
}

pub fn load_messages(path: &Path) -> Result<Vec<Message>> {
    let mut messages = Vec::new();
    for_each_session_entry(path, |_, entry| {
        match entry {
            SessionEntry::Message { message, .. } => messages.push(message),
            SessionEntry::Compaction { summary, .. } => {
                messages.clear();
                messages.push(Message::text(
                    Role::User,
                    format!(
                        "{}\n\n<summary>\n{}\n</summary>",
                        crate::agent::messages::COMPACTION_SUMMARY_PREFIX,
                        summary.trim()
                    ),
                ));
            }
            SessionEntry::Header { .. } | SessionEntry::Metadata { .. } => {}
        }
        Ok(true)
    })?;
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
        .into_iter()
        .find(|info| info.message_count > 0 || info.title != "(empty session)")
        .map(|info| info.path))
}

pub fn list_sessions_for_cwd(dir: &Path, cwd: &Path) -> Result<Vec<SessionInfo>> {
    let cwd = canonical_display_path(cwd);
    let mut sessions = list_sessions(dir)?
        .into_iter()
        .filter(|session| {
            session
                .cwd
                .as_deref()
                .is_some_and(|session_cwd| canonical_display_str(session_cwd) == cwd)
        })
        .collect::<Vec<_>>();
    sort_sessions_newest_first(&mut sessions);
    Ok(sessions)
}

pub fn list_sessions(dir: &Path) -> Result<Vec<SessionInfo>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    let mut diagnostics = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if diagnostics < 10 {
                    eprintln!("[session] skipped unreadable directory entry: {error}");
                }
                diagnostics += 1;
                continue;
            }
        };
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        match session_info_with_diagnostics(&path, false) {
            Ok(Some(info)) => sessions.push(info),
            Ok(None) => {
                if diagnostics < 10 {
                    eprintln!("[session] skipped invalid session {}", path.display());
                }
                diagnostics += 1;
            }
            Err(error) => {
                if diagnostics < 10 {
                    eprintln!(
                        "[session] skipped unreadable session {}: {error}",
                        path.display()
                    );
                }
                diagnostics += 1;
            }
        }
    }
    if diagnostics > 10 {
        eprintln!(
            "[session] omitted {} additional session discovery diagnostics",
            diagnostics - 10
        );
    }
    sort_sessions_newest_first(&mut sessions);
    Ok(sessions)
}

#[derive(Debug, thiserror::Error)]
pub enum SessionRefError {
    #[error("no session matches '{0}'")]
    NoMatch(String),
    #[error("session reference '{0}' is ambiguous")]
    Ambiguous(String),
}

pub fn resolve_session_ref(dir: &Path, _cwd: &Path, reference: &str) -> Result<PathBuf> {
    let path = PathBuf::from(reference);
    if is_explicit_session_path(reference, &path) {
        return Ok(path);
    }
    let matches = list_sessions(dir)?
        .into_iter()
        .filter(|session| {
            session.id.starts_with(reference) || session.short_id.starts_with(reference)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session.path.clone()),
        [] => Err(SessionRefError::NoMatch(reference.to_string()).into()),
        _ => Err(SessionRefError::Ambiguous(reference.to_string()).into()),
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
    let archive_cutoff = latest_compaction_line(path)?.unwrap_or(0);
    let mut out = String::new();
    let mut count = 0usize;

    for_each_session_entry(path, |line_number, entry| {
        let Some((role, text)) = searchable_entry_text(&entry) else {
            return Ok(true);
        };
        let Some((start, end)) = matcher.find(&text) else {
            return Ok(true);
        };
        let status = if line_number < archive_cutoff {
            "archived"
        } else {
            "active"
        };
        let snippet = history_snippet(&text, start, end, HISTORY_SNIPPET_CHARS);
        out.push_str(&format!("line {line_number} {status} {role}: {snippet}\n"));
        count += 1;
        Ok(count < limit)
    })?;

    if out.is_empty() {
        out.push_str("no history matches\n");
    }
    Ok(out)
}

pub fn read_history(path: &Path, offset: usize, limit: usize) -> Result<String> {
    let offset = offset.max(1);
    let limit = limit.clamp(1, 100);
    let archive_cutoff = latest_compaction_line(path)?.unwrap_or(0);
    let mut out = String::new();
    let mut count = 0usize;

    for_each_session_entry(path, |line_number, entry| {
        if line_number < offset {
            return Ok(true);
        }
        let status = if line_number < archive_cutoff {
            "archived"
        } else {
            "active"
        };
        match render_history_entry(&entry) {
            Some((kind, text)) => {
                out.push_str(&format!("line {line_number} {status} {kind}:\n"));
                for line in text.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            None => out.push_str(&format!("line {line_number} {status}: metadata\n")),
        }
        count += 1;
        Ok(count < limit)
    })?;

    if out.is_empty() {
        out.push_str("no history lines\n");
    }
    Ok(out)
}

fn latest_compaction_line(path: &Path) -> Result<Option<usize>> {
    let mut latest = None;
    for_each_session_entry(path, |line_number, entry| {
        if matches!(entry, SessionEntry::Compaction { .. }) {
            latest = Some(line_number);
        }
        Ok(true)
    })?;
    Ok(latest)
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

#[derive(Debug)]
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
    color_mode: Option<String>,
    diff_mode: Option<String>,
    safety: Option<String>,
    tools: Option<Vec<String>>,
) -> Result<SessionRefResolution> {
    match resolve_session_ref(dir, cwd, reference) {
        Ok(path) => Ok(SessionRefResolution::Existing(path)),
        Err(error)
            if matches!(
                error.downcast_ref::<SessionRefError>(),
                Some(SessionRefError::NoMatch(_))
            ) && is_valid_user_session_id(reference) =>
        {
            let path = dir.join(format!("{reference}.jsonl"));
            if path.exists() {
                return Ok(SessionRefResolution::Existing(path));
            }
            JsonlSession::create_named_with_color_mode(
                dir.to_path_buf(),
                reference,
                provider,
                model,
                thinking,
                color_mode,
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
    session_info_with_diagnostics(path, true)
}

fn session_info_with_diagnostics(path: &Path, diagnose: bool) -> Result<Option<SessionInfo>> {
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

    for_each_session_entry_with_diagnostics(path, diagnose, |_, entry| {
        match entry {
            SessionEntry::Header {
                id: header_id,
                provider: header_provider,
                model: header_model,
                thinking: header_thinking,
                color_mode: header_color_mode,
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
                explicit_color_mode = header_color_mode;
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
        Ok(true)
    })?;

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

fn is_explicit_session_path(reference: &str, path: &Path) -> bool {
    reference.contains('/')
        || reference.starts_with("./")
        || reference.starts_with("../")
        || reference.starts_with('/')
        || path.exists()
}

fn canonical_display_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn canonical_display_str(path: &str) -> String {
    canonical_display_path(Path::new(path))
}

fn validate_session_file(path: &Path) -> Result<()> {
    let mut first = None;
    for_each_session_entry(path, |_, entry| {
        first = Some(entry);
        Ok(false)
    })?;
    match first {
        Some(SessionEntry::Header { .. }) => Ok(()),
        Some(_) => anyhow::bail!("{} is not a Ferrum session: missing header", path.display()),
        None => anyhow::bail!(
            "{} is not a Ferrum session: empty or corrupt file",
            path.display()
        ),
    }
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
    fn named_session_created_in_dir_a_resolves_from_dir_b() {
        let temp = tempfile::tempdir().unwrap();
        let dir_a = temp.path().join("a");
        let dir_b = temp.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        write_test_header(
            &temp.path().join("global-name.jsonl"),
            "global-name",
            Some(&dir_a),
        );

        let resolved = resolve_session_ref(temp.path(), &dir_b, "global-name").unwrap();

        assert_eq!(resolved, temp.path().join("global-name.jsonl"));
    }

    #[test]
    fn ambiguous_global_session_prefix_errors() {
        let temp = tempfile::tempdir().unwrap();
        write_test_header(&temp.path().join("abc-one.jsonl"), "abc-one", None);
        write_test_header(&temp.path().join("abc-two.jsonl"), "abc-two", None);
        let error = resolve_session_ref(temp.path(), temp.path(), "abc").unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn session_ref_ending_jsonl_creates_named_session_when_not_path() {
        let temp = tempfile::tempdir().unwrap();
        let resolution = resolve_or_create_session_ref(
            temp.path(),
            temp.path(),
            "foo.jsonl",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        match resolution {
            SessionRefResolution::Created(path) => {
                assert_eq!(path, temp.path().join("foo.jsonl.jsonl"));
                assert!(path.exists());
            }
            SessionRefResolution::Existing(_) => panic!("expected named session creation"),
        }
    }

    #[test]
    fn opening_non_session_file_does_not_chmod_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("not-session.jsonl");
        std::fs::write(&path, "not json\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = JsonlSession::open(path.clone()).unwrap_err();

        assert!(error.to_string().contains("not a Ferrum session"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn same_directory_via_symlink_resolves_same_latest_session() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_test_header(&temp.path().join("linked.jsonl"), "linked", Some(&link));

        let sessions = list_sessions_for_cwd(temp.path(), &real).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "linked");
    }

    fn write_test_header(path: &Path, id: &str, cwd: Option<&Path>) {
        let header = SessionEntry::Header {
            id: id.to_string(),
            parent_id: None,
            timestamp_ms: 1,
            version: 1,
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
            cwd: cwd.map(|path| path.display().to_string()),
        };
        let text = serde_json::to_string(&header).unwrap();
        std::fs::write(path, format!("{text}\n")).unwrap();
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
        assert_eq!(messages[0].role, Role::User);
        assert!(
            messages[0]
                .text_content()
                .starts_with(crate::agent::messages::COMPACTION_SUMMARY_PREFIX)
        );
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
    fn session_message_with_old_usage_without_source_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("old-usage.jsonl");
        let header = serde_json::to_string(&SessionEntry::Header {
            id: "old-usage".to_string(),
            parent_id: None,
            timestamp_ms: 1,
            version: 1,
            provider: None,
            model: None,
            thinking: None,
            color_mode: None,
            diff_mode: None,
            safety: None,
            tools: None,
            cwd: None,
        })
        .unwrap();
        let message = r#"{"type":"message","id":"m1","parent_id":null,"timestamp_ms":2,"message":{"role":"assistant","content":[{"type":"text","text":"old usage"}],"usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#;
        std::fs::write(&path, format!("{header}\n{message}\n")).unwrap();

        let messages = load_messages(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].usage.as_ref().unwrap().source, "unknown");
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
    fn preserves_header_only_session_for_crash_safe_discovery() {
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
        session.sync_checkpoint().unwrap();
        assert!(path.exists());
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
        session.sync_checkpoint().unwrap();
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
    fn stores_initial_color_mode_in_header() {
        let temp = tempfile::tempdir().unwrap();
        let session = JsonlSession::create_with_color_mode(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            Some("off".to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        let info = session_info(session.path()).unwrap().unwrap();
        assert_eq!(info.color_mode.as_deref(), Some("off"));
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
    fn ambiguous_reference_never_creates_a_session() {
        let temp = tempfile::tempdir().unwrap();
        write_test_header(&temp.path().join("abc-one.jsonl"), "abc-one", None);
        write_test_header(&temp.path().join("abc-two.jsonl"), "abc-two", None);

        let error = resolve_or_create_session_ref(
            temp.path(),
            temp.path(),
            "abc",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
        assert!(!temp.path().join("abc.jsonl").exists());
    }

    #[test]
    fn latest_session_skips_abandoned_anonymous_header() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let mut useful = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        useful
            .append_message(&Message::text(Role::User, "keep me"))
            .unwrap();
        let useful_path = useful.path().clone();
        drop(useful);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let abandoned = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        drop(abandoned);

        assert_eq!(
            latest_session_for_cwd(temp.path(), &cwd).unwrap(),
            Some(useful_path)
        );
    }

    #[test]
    fn session_open_repairs_incomplete_tail_before_append() {
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
        let path = session.path().clone();
        drop(session);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"type":"message""#)
            .unwrap();

        let mut reopened = JsonlSession::open(path.clone()).unwrap();
        reopened
            .append_message(&Message::text(Role::User, "after repair"))
            .unwrap();

        let messages = load_messages(&path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text_content(), "after repair");
        assert!(fs::read(&path).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn unreadable_session_entry_does_not_break_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let valid = JsonlSession::create(
            temp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        std::fs::create_dir(temp.path().join("broken.jsonl")).unwrap();

        let sessions = list_sessions(temp.path()).unwrap();

        assert!(sessions.iter().any(|info| info.path == *valid.path()));
    }

    #[test]
    fn anonymous_session_filenames_are_unique_under_concurrency() {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::sync::Arc::new(temp.path().to_path_buf());
        let mut threads = Vec::new();
        for _ in 0..64 {
            let dir = std::sync::Arc::clone(&dir);
            threads.push(std::thread::spawn(move || {
                JsonlSession::create((*dir).clone(), None, None, None, None, None, None)
                    .unwrap()
                    .path()
                    .clone()
            }));
        }
        let paths = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 64);
    }

    #[test]
    fn session_append_process_child() {
        let Ok(path) = std::env::var("FERRUM_SESSION_STRESS_PATH") else {
            return;
        };
        let writer = std::env::var("FERRUM_SESSION_STRESS_WRITER").unwrap();
        let mut session = JsonlSession::open(PathBuf::from(path)).unwrap();
        for index in 0..32 {
            session
                .append_message(&Message::text(
                    Role::User,
                    format!("writer {writer} record {index}"),
                ))
                .unwrap();
        }
    }

    #[test]
    fn multiprocess_session_appends_remain_valid_jsonl() {
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
        let path = session.path().clone();
        drop(session);
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for writer in 0..8 {
            children.push(
                std::process::Command::new(&executable)
                    .arg("--exact")
                    .arg("session::jsonl::tests::session_append_process_child")
                    .arg("--nocapture")
                    .env("FERRUM_SESSION_STRESS_PATH", &path)
                    .env("FERRUM_SESSION_STRESS_WRITER", writer.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }

        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.lines()
                .all(|line| serde_json::from_str::<SessionEntry>(line).is_ok())
        );
        assert_eq!(load_messages(&path).unwrap().len(), 8 * 32);
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
