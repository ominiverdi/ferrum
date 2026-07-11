use anyhow::{Context, Result};
use std::{collections::BinaryHeap, fs, path::Path};

const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 10_000;

pub fn list(path: &Path, limit: Option<usize>) -> Result<String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut selected = BinaryHeap::with_capacity(limit.saturating_add(1));
    let mut entry_count = 0usize;

    for entry in fs::read_dir(path).with_context(|| format!("failed to list {}", path.display()))? {
        let entry = entry?;
        entry_count = entry_count.saturating_add(1);
        let file_type = entry.file_type()?;
        let suffix = if file_type.is_dir() { "/" } else { "" };
        let rendered = format!("{}{}", entry.file_name().to_string_lossy(), suffix);
        let key = (rendered.to_lowercase(), rendered);
        if selected.len() < limit {
            selected.push(key);
        } else if selected.peek().is_some_and(|largest| key < *largest) {
            selected.pop();
            selected.push(key);
        }
    }

    if selected.is_empty() {
        return Ok("(empty directory)".to_string());
    }
    let mut entries = selected
        .into_iter()
        .map(|(_, rendered)| rendered)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.to_lowercase());
    let mut output = entries.join("\n");
    if entry_count > limit {
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

    #[test]
    fn bounded_selection_keeps_lexicographically_smallest_entries() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["z", "y", "c", "b", "a"] {
            std::fs::write(temp.path().join(name), "").unwrap();
        }

        let output = list(temp.path(), Some(2)).unwrap();
        assert_eq!(output.lines().take(2).collect::<Vec<_>>(), ["a", "b"]);
    }
}
