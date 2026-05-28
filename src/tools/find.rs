use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const MAX_RESULTS: usize = 1000;

pub fn find(root: &Path, name: Option<&str>, extension: Option<&str>) -> Result<String> {
    let mut results = Vec::new();
    visit(root, name, extension, &mut results)?;
    results.sort();
    if results.is_empty() {
        return Ok("no matches".to_string());
    }
    let truncated = results.len() > MAX_RESULTS;
    results.truncate(MAX_RESULTS);
    let mut output = results
        .into_iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if truncated {
        output.push_str(&format!("\n[truncated to {MAX_RESULTS} results]"));
    }
    Ok(output)
}

fn visit(
    path: &Path,
    name: Option<&str>,
    extension: Option<&str>,
    results: &mut Vec<PathBuf>,
) -> Result<()> {
    if results.len() > MAX_RESULTS {
        return Ok(());
    }
    if path.is_dir() {
        for entry in
            std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with('.') || file_name == "target" {
                continue;
            }
            visit(&entry.path(), name, extension, results)?;
        }
        return Ok(());
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let name_matches = name.is_none_or(|needle| file_name.contains(needle));
    let extension_matches = extension.is_none_or(|needle| ext == needle.trim_start_matches('.'));
    if name_matches && extension_matches {
        results.push(path.to_path_buf());
    }
    Ok(())
}
