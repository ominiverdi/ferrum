use anyhow::{Context, Result};
use std::{fs, path::Path};

const MAX_BYTES: usize = 50 * 1024;

pub fn read_text(path: &Path, offset: usize, limit: Option<usize>) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines = text.lines().skip(offset.saturating_sub(1));
    let mut output = match limit {
        Some(limit) => lines.take(limit).collect::<Vec<_>>().join("\n"),
        None => lines.collect::<Vec<_>>().join("\n"),
    };
    if output.len() > MAX_BYTES {
        output.truncate(MAX_BYTES);
        output.push_str("\n[truncated]");
    }
    Ok(output)
}
