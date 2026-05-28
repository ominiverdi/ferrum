use anyhow::{Context, Result};
use std::{collections::HashSet, fs, path::Path};

const MAX_CONTEXT_BYTES: usize = 128 * 1024;

pub fn load_context(config_dir: &Path, cwd: &Path) -> Result<Option<String>> {
    let mut paths = Vec::new();
    paths.push(config_dir.join("AGENTS.md"));

    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut ancestors = canonical_cwd.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        paths.push(ancestor.join("AGENTS.md"));
    }

    let mut seen = HashSet::new();
    let mut sections = Vec::new();
    let mut total = 0usize;

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
        let section = format!("# {}\n\n{}", display_path(&path, cwd), text.trim());
        total += section.len();
        sections.push(section);
        if total >= MAX_CONTEXT_BYTES {
            break;
        }
    }

    if sections.is_empty() {
        return Ok(None);
    }

    let mut context = format!(
        "Project and user instructions from AGENTS.md files. Follow later, more specific files when instructions conflict.\n\n{}",
        sections.join("\n\n---\n\n")
    );
    if context.len() > MAX_CONTEXT_BYTES {
        context.truncate(MAX_CONTEXT_BYTES);
        context.push_str("\n\n[AGENTS.md context truncated]");
    }
    Ok(Some(context))
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
        fs::write(repo.join("AGENTS.md"), "project").unwrap();

        let context = load_context(&config, &repo).unwrap().unwrap();
        assert!(context.contains("global"));
        assert!(context.contains("project"));
        assert!(context.find("global").unwrap() < context.find("project").unwrap());
    }
}
