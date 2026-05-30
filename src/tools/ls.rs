use anyhow::{Context, Result};
use std::{fs, path::Path};

const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 10_000;

pub fn list(path: &Path, limit: Option<usize>) -> Result<String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut entries = Vec::new();
    let mut limit_reached = false;
    for entry in fs::read_dir(path).with_context(|| format!("failed to list {}", path.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let suffix = if file_type.is_dir() { "/" } else { "" };
        entries.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
    }
    entries.sort_by_key(|entry| entry.to_lowercase());
    if entries.len() > limit {
        entries.truncate(limit);
        limit_reached = true;
    }
    if entries.is_empty() {
        return Ok("(empty directory)".to_string());
    }
    let mut output = entries.join("\n");
    if limit_reached {
        output.push_str(&format!(
            "\n\n[{limit} entries limit reached. Use limit={} for more]",
            limit.saturating_mul(2).min(MAX_LIMIT)
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_dotfiles_and_directory_suffixes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".env"), "").unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();

        let output = list(temp.path(), None).unwrap();
        assert!(output.contains(".env"));
        assert!(output.contains("src/"));
    }

    #[test]
    fn applies_entry_limit() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a"), "").unwrap();
        std::fs::write(temp.path().join("b"), "").unwrap();

        let output = list(temp.path(), Some(1)).unwrap();
        assert!(output.contains("entries limit reached"));
        assert_eq!(output.lines().next().unwrap(), "a");
    }
}
