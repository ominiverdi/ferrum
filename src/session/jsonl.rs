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
    pub fn create(dir: PathBuf) -> Result<Self> {
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
            provider: None,
            model: None,
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

pub fn latest_session(dir: &Path) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut entries = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries.pop())
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
        let mut session = JsonlSession::create(temp.path().to_path_buf()).unwrap();
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
