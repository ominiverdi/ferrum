use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use std::path::Path;

const DEFAULT_LIMIT: usize = 1000;
const MAX_LIMIT: usize = 10_000;

#[derive(Debug, Clone)]
pub struct FindOptions<'a> {
    pub pattern: Option<&'a str>,
    pub name: Option<&'a str>,
    pub extension: Option<&'a str>,
    pub limit: Option<usize>,
}

pub fn find(root: &Path, options: FindOptions<'_>) -> Result<String> {
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matcher = build_matcher(options.pattern)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", root.display()))?;

    let mut results = Vec::new();
    let mut limit_reached = false;
    let mut walker = WalkBuilder::new(&root);
    walker
        .standard_filters(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .ignore(true)
        .filter_entry(|entry| !should_skip_entry(entry.path()));

    for entry in walker.build() {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        let path = entry.path();
        if path == root || !path.is_file() {
            continue;
        }
        if matches_path(path, &root, matcher.as_ref(), &options) {
            results.push(relative_display(path, &root));
            if results.len() >= limit {
                limit_reached = true;
                break;
            }
        }
    }

    results.sort_by_key(|value| value.to_lowercase());
    if results.is_empty() {
        return Ok("no matches".to_string());
    }

    let mut output = results.join("\n");
    if limit_reached {
        output.push_str(&format!(
            "\n\n[{limit} results limit reached. Use limit={} for more, or refine pattern]",
            limit.saturating_mul(2).min(MAX_LIMIT)
        ));
    }
    Ok(output)
}

fn build_matcher(pattern: Option<&str>) -> Result<Option<GlobSet>> {
    let Some(pattern) = pattern.filter(|pattern| !pattern.trim().is_empty()) else {
        return Ok(None);
    };
    let mut builder = GlobSetBuilder::new();
    builder.add(
        Glob::new(pattern)
            .or_else(|_| Glob::new(&format!("**/{pattern}")))
            .with_context(|| format!("invalid glob pattern: {pattern}"))?,
    );
    if !pattern.starts_with("**/") && !pattern.contains('/') {
        builder.add(Glob::new(&format!("**/{pattern}"))?);
    }
    Ok(Some(builder.build()?))
}

fn matches_path(
    path: &Path,
    root: &Path,
    matcher: Option<&GlobSet>,
    options: &FindOptions<'_>,
) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");

    let pattern_matches = matcher.is_none_or(|matcher| matcher.is_match(relative));
    let name_matches = options.name.is_none_or(|needle| file_name.contains(needle));
    let extension_matches = options
        .extension
        .is_none_or(|needle| ext == needle.trim_start_matches('.'));
    pattern_matches && name_matches && extension_matches
}

fn relative_display(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn should_skip_entry(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    matches!(name, ".git" | "target" | "node_modules")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_hidden_config_directories() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".config/systemd/user");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("example.service"), "service").unwrap();

        let output = find(
            temp.path(),
            FindOptions {
                pattern: Some("**/*.service"),
                name: None,
                extension: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(output.contains(".config/systemd/user/example.service"));
    }

    #[test]
    fn skips_noisy_dependency_directories() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("node_modules/pkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("example.service"), "service").unwrap();

        let output = find(
            temp.path(),
            FindOptions {
                pattern: Some("**/*.service"),
                name: None,
                extension: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(output, "no matches");
    }

    #[test]
    fn supports_legacy_name_and_extension_filters() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("openai.rs"), "").unwrap();
        std::fs::write(temp.path().join("other.txt"), "").unwrap();

        let output = find(
            temp.path(),
            FindOptions {
                pattern: None,
                name: Some("openai"),
                extension: Some("rs"),
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(output, "openai.rs");
    }

    #[test]
    fn respects_gitignore() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(temp.path().join("ignored.log"), "").unwrap();
        std::fs::write(temp.path().join("kept.log"), "").unwrap();

        let output = find(
            temp.path(),
            FindOptions {
                pattern: Some("*.log"),
                name: None,
                extension: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(output, "kept.log");
    }
}
