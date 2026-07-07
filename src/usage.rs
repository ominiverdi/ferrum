use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageRecord {
    pub timestamp_unix: u64,
    pub provider: String,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub source: String,
}

impl UsageRecord {
    fn normalized_total(&self) -> u64 {
        self.total_tokens.unwrap_or_else(|| {
            self.input_tokens
                .unwrap_or(0)
                .saturating_add(self.output_tokens.unwrap_or(0))
                .saturating_add(self.cache_read_tokens)
                .saturating_add(self.cache_write_tokens)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsagePeriod {
    Day,
    Week,
    Month,
}

impl UsagePeriod {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("day") {
            "day" | "daily" => Ok(Self::Day),
            "week" | "weekly" => Ok(Self::Week),
            "month" | "monthly" => Ok(Self::Month),
            other => anyhow::bail!("usage: /usage [day|week|month], got: {other}"),
        }
    }

    pub fn seconds(self) -> u64 {
        match self {
            Self::Day => 24 * 60 * 60,
            Self::Week => 7 * 24 * 60 * 60,
            Self::Month => 30 * 24 * 60 * 60,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "last 24h",
            Self::Week => "last 7d",
            Self::Month => "last 30d",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSummary {
    pub requests: u64,
    pub provider_records: u64,
    pub estimated_records: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSummaryRow {
    pub provider: String,
    pub model: String,
    pub summary: UsageSummary,
}

pub fn append_usage_record(data_dir: &Path, record: &UsageRecord) -> Result<()> {
    let path = usage_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        tighten_dir_permissions(parent);
    }
    tighten_file_permissions(&path);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let json = serde_json::to_string(record)?;
    writeln!(file, "{json}").with_context(|| format!("failed to write {}", path.display()))
}

pub fn summarize_usage(
    data_dir: &Path,
    period: UsagePeriod,
    now: u64,
) -> Result<Vec<UsageSummaryRow>> {
    let since = now.saturating_sub(period.seconds());
    let records = read_usage_records(data_dir)?;
    let mut grouped: BTreeMap<(String, String), UsageSummary> = BTreeMap::new();
    for record in records
        .into_iter()
        .filter(|record| record.timestamp_unix >= since && record.timestamp_unix <= now)
    {
        let summary = grouped
            .entry((record.provider.clone(), record.model.clone()))
            .or_default();
        summary.requests = summary.requests.saturating_add(1);
        if record.source == "provider" {
            summary.provider_records = summary.provider_records.saturating_add(1);
        } else if record.source == "estimated" {
            summary.estimated_records = summary.estimated_records.saturating_add(1);
        }
        summary.input_tokens = summary
            .input_tokens
            .saturating_add(record.input_tokens.unwrap_or(0));
        summary.output_tokens = summary
            .output_tokens
            .saturating_add(record.output_tokens.unwrap_or(0));
        summary.cache_read_tokens = summary
            .cache_read_tokens
            .saturating_add(record.cache_read_tokens);
        summary.cache_write_tokens = summary
            .cache_write_tokens
            .saturating_add(record.cache_write_tokens);
        summary.total_tokens = summary
            .total_tokens
            .saturating_add(record.normalized_total());
    }
    Ok(grouped
        .into_iter()
        .map(|((provider, model), summary)| UsageSummaryRow {
            provider,
            model,
            summary,
        })
        .collect())
}

fn read_usage_records(data_dir: &Path) -> Result<Vec<UsageRecord>> {
    let path = usage_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<UsageRecord>(&line) {
            records.push(record);
        }
    }
    Ok(records)
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn usage_path(data_dir: &Path) -> PathBuf {
    data_dir.join("usage.jsonl")
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

    #[test]
    fn usage_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let record = UsageRecord {
            timestamp_unix: 1,
            provider: "p".to_string(),
            model: "m".to_string(),
            input_tokens: Some(2),
            output_tokens: Some(3),
            total_tokens: Some(5),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            source: "test".to_string(),
        };
        append_usage_record(temp.path(), &record).unwrap();
        let mode = fs::metadata(temp.path().join("usage.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn appends_usage_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let record = UsageRecord {
            timestamp_unix: 1,
            provider: "p".to_string(),
            model: "m".to_string(),
            input_tokens: Some(2),
            output_tokens: Some(3),
            total_tokens: Some(5),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            source: "test".to_string(),
        };
        append_usage_record(temp.path(), &record).unwrap();
        let text = fs::read_to_string(temp.path().join("usage.jsonl")).unwrap();
        assert!(text.contains("\"provider\":\"p\""));
    }

    #[test]
    fn summarizes_usage_by_provider_and_model() {
        let temp = tempfile::tempdir().unwrap();
        append_usage_record(
            temp.path(),
            &UsageRecord {
                timestamp_unix: 100,
                provider: "p".to_string(),
                model: "m".to_string(),
                input_tokens: Some(2),
                output_tokens: Some(3),
                total_tokens: None,
                cache_read_tokens: 4,
                cache_write_tokens: 5,
                source: "test".to_string(),
            },
        )
        .unwrap();
        let rows = summarize_usage(temp.path(), UsagePeriod::Day, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].summary.requests, 1);
        assert_eq!(rows[0].summary.estimated_records, 0);
        assert_eq!(rows[0].summary.provider_records, 0);
        assert_eq!(rows[0].summary.total_tokens, 14);
    }
}
