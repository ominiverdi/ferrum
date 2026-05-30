use crate::agent::messages::{Message, Role};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
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
}

impl JsonlSession {
    pub fn create(dir: PathBuf, provider: Option<String>, model: Option<String>) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let path = dir.join(format!("{}.jsonl", now_ms()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut session = Self { path, file };
        session.append(&SessionEntry::Header {
            id: Uuid::new_v4().to_string(),
            parent_id: None,
            timestamp_ms: now_ms(),
            version: 1,
            provider,
            model,
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
        let file = OpenOptions::new()
            .append(true)
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

pub fn load_messages(path: &Path) -> Result<Vec<Message>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry =
            serde_json::from_str(&line).context("failed to parse session entry")?;
        match entry {
            SessionEntry::Message { message, .. } => messages.push(message),
            SessionEntry::Compaction { summary, .. } => messages.push(Message::text(
                Role::System,
                format!("Conversation summary from previous compaction:\n{summary}"),
            )),
            SessionEntry::Header { .. } => {}
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
    pub title: String,
    pub message_count: usize,
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

fn session_info(path: &Path) -> Result<Option<SessionInfo>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH);
    let mut id = None;
    let mut cwd = None;
    let mut provider = None;
    let mut model = None;
    let mut title = None;
    let mut message_count = 0usize;

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
                cwd: header_cwd,
                ..
            } => {
                id = Some(header_id);
                provider = header_provider;
                model = header_model;
                cwd = header_cwd;
            }
            SessionEntry::Message { message, .. } => {
                message_count += 1;
                if title.is_none() && matches!(message.role, Role::User) {
                    let text = message.text_content();
                    if !text.trim().is_empty() {
                        title = Some(one_line_title(&text));
                    }
                }
            }
            SessionEntry::Compaction { .. } => {}
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
        provider,
        model,
        title: title.unwrap_or_else(|| "(empty session)".to_string()),
        message_count,
        modified,
    }))
}

fn sort_sessions_newest_first(sessions: &mut [SessionInfo]) {
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
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
        .expect("system time is before unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::messages::{Message, Role};

    #[test]
    fn writes_header_and_message_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = JsonlSession::create(temp.path().to_path_buf(), None, None).unwrap();
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
}
