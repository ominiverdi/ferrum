use anyhow::{Context, Result};
use std::{fs, path::Path};

pub fn list(path: &Path) -> Result<String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to list {}", path.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let suffix = if file_type.is_dir() { "/" } else { "" };
        entries.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
    }
    entries.sort();
    Ok(entries.join("\n"))
}
