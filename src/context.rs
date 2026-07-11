use crate::text_truncate::truncate_to_max_bytes;
use anyhow::{Context, Result};
use std::{collections::HashSet, fs::File, io::Read, path::Path};

const MAX_CONTEXT_BYTES: usize = 128 * 1024;
const CONTEXT_PREFIX: &str = "Project and user instructions from AGENTS.md files. Follow later, more specific files when instructions conflict.\n\n";
const CONTEXT_SEPARATOR: &str = "\n\n---\n\n";
const CONTEXT_TRUNCATED_MARKER: &str = "\n\n[AGENTS.md context truncated]";

pub fn load_context(config_dir: &Path, cwd: &Path) -> Result<Option<String>> {
    let mut paths = Vec::new();
    push_context_candidates(&mut paths, config_dir);

    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut ancestors = canonical_cwd.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        push_context_candidates(&mut paths, ancestor);
    }

    let mut seen = HashSet::new();
    let mut selected_newest_first = Vec::new();
    let max_without_marker = MAX_CONTEXT_BYTES.saturating_sub(CONTEXT_TRUNCATED_MARKER.len());
    let mut used = CONTEXT_PREFIX.len();
    let mut omitted = false;

    for path in paths.into_iter().rev() {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen.insert(key) || !path.exists() {
            continue;
        }

        let separator_len = if selected_newest_first.is_empty() {
            0
        } else {
            CONTEXT_SEPARATOR.len()
        };
        let header = format!("# {}\n\n", display_path(&path, cwd));
        let available = max_without_marker
            .saturating_sub(used)
            .saturating_sub(separator_len)
            .saturating_sub(header.len());
        if available == 0 {
            omitted = true;
            break;
        }

        let (text, truncated) = read_bounded_utf8(&path, available)?;
        let text = text.trim();
        if text.is_empty() {
            omitted |= truncated;
            continue;
        }
        let mut section = header;
        section.push_str(text);
        used = used
            .saturating_add(separator_len)
            .saturating_add(section.len());
        selected_newest_first.push(section);
        if truncated {
            omitted = true;
            break;
        }
    }

    if selected_newest_first.is_empty() {
        return Ok(None);
    }

    selected_newest_first.reverse();
    let mut context = format!(
        "{CONTEXT_PREFIX}{}",
        selected_newest_first.join(CONTEXT_SEPARATOR)
    );
    if omitted {
        let available = MAX_CONTEXT_BYTES.saturating_sub(CONTEXT_TRUNCATED_MARKER.len());
        context = truncate_to_max_bytes(&context, available);
        context.push_str(CONTEXT_TRUNCATED_MARKER);
    }
    Ok(Some(context))
}

fn read_bounded_utf8(path: &Path, limit: usize) -> Result<(String, bool)> {
    let file = File::open(path)
        .with_context(|| format!("failed to read context file {}", path.display()))?;
    let mut bytes = Vec::with_capacity(limit.min(16 * 1024).saturating_add(1));
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read context file {}", path.display()))?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);

    if truncated {
        while std::str::from_utf8(&bytes).is_err_and(|error| error.error_len().is_none()) {
            bytes.pop();
        }
    }
    let text = String::from_utf8(bytes)
        .with_context(|| format!("context file is not UTF-8: {}", path.display()))?;
    Ok((text, truncated))
}

fn push_context_candidates(paths: &mut Vec<std::path::PathBuf>, dir: &Path) {
    paths.push(dir.join("AGENTS.md"));
    paths.push(dir.join("agents.md"));
}

fn display_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_global_and_project_context() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(config.join("AGENTS.md"), "global").unwrap();
        fs::write(repo.join("agents.md"), "project").unwrap();

        let context = load_context(&config, &repo).unwrap().unwrap();
        assert!(context.contains("global"));
        assert!(context.contains("project"));
        assert!(context.find("global").unwrap() < context.find("project").unwrap());
    }

    #[test]
    fn huge_parent_agents_does_not_drop_small_cwd_agents() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let repo = temp.path().join("repo");
        let nested = repo.join("a/b/c");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            repo.join("AGENTS.md"),
            format!("parent {}", "x".repeat(MAX_CONTEXT_BYTES * 2)),
        )
        .unwrap();
        fs::write(nested.join("AGENTS.md"), "cwd-specific-instruction").unwrap();

        let context = load_context(&config, &nested).unwrap().unwrap();

        assert!(context.contains("cwd-specific-instruction"));
        assert!(context.contains("AGENTS.md context truncated"));
        assert!(context.len() <= MAX_CONTEXT_BYTES);
    }

    #[test]
    fn does_not_read_invalid_data_beyond_remaining_budget() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let mut bytes = vec![b'x'; MAX_CONTEXT_BYTES * 2];
        bytes.push(0xff);
        fs::write(repo.join("AGENTS.md"), bytes).unwrap();

        let context = load_context(&config, &repo).unwrap().unwrap();
        assert!(context.contains("AGENTS.md context truncated"));
        assert!(context.len() <= MAX_CONTEXT_BYTES);
    }
}
