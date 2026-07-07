use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

const MAX_BYTES: usize = 50 * 1024;

pub fn read_text(path: &Path, offset: usize, limit: Option<usize>) -> Result<String> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut output = String::new();
    let start = offset.saturating_sub(1);
    for line in reader.lines().skip(start) {
        if let Some(limit) = limit
            && output.lines().count() >= limit
        {
            break;
        }
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
        if output.len() > MAX_BYTES {
            output.truncate(MAX_BYTES);
            output.push_str("\n[truncated]");
            break;
        }
    }
    Ok(output)
}
