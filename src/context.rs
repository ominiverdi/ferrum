use crate::text_truncate::truncate_to_max_bytes;
use anyhow::{Context, Result};
use std::{collections::HashSet, fs, path::Path};

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
    let mut sections = Vec::new();

    for path in paths {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen.insert(key) || !path.exists() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read context file {}", path.display()))?;
        if text.trim().is_empty() {
            continue;
        }
        sections.push(format!("# {}\n\n{}", display_path(&path, cwd), text.trim()));
    }

    if sections.is_empty() {
        return Ok(None);
    }

    let selected = select_context_sections(sections);
    let mut context = format!("{CONTEXT_PREFIX}{}", selected.join(CONTEXT_SEPARATOR));
    if context.len() > MAX_CONTEXT_BYTES {
        context = truncate_to_max_bytes(
            &context,
            MAX_CONTEXT_BYTES.saturating_sub(CONTEXT_TRUNCATED_MARKER.len()),
        );
        context.push_str(CONTEXT_TRUNCATED_MARKER);
    }
    Ok(Some(context))
}

fn select_context_sections(sections: Vec<String>) -> Vec<String> {
    let mut selected_newest_first = Vec::new();
    let mut used = CONTEXT_PREFIX.len();
    let marker_budget = CONTEXT_TRUNCATED_MARKER.len();
    let max_without_marker = MAX_CONTEXT_BYTES.saturating_sub(marker_budget);
    let mut omitted = false;

    for section in sections.into_iter().rev() {
        let separator = if selected_newest_first.is_empty() {
            0
        } else {
            CONTEXT_SEPARATOR.len()
        };
        let needed = separator.saturating_add(section.len());
        if used.saturating_add(needed) <= max_without_marker {
            used = used.saturating_add(needed);
            selected_newest_first.push(section);
            continue;
        }

        omitted = true;
        if selected_newest_first.is_empty() {
            let available = max_without_marker.saturating_sub(used);
            selected_newest_first.push(truncate_to_max_bytes(&section, available));
            break;
        }
    }

    selected_newest_first.reverse();
    if omitted {
        selected_newest_first.push(CONTEXT_TRUNCATED_MARKER.trim().to_string());
    }
    selected_newest_first
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
        assert!(context.len() <= MAX_CONTEXT_BYTES + CONTEXT_TRUNCATED_MARKER.len());
    }
}
