use crate::text_truncate::truncate_tail_to_max_bytes;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::{path::Path, process::Command};

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 10_000;
const MAX_LINE_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct GrepOptions<'a> {
    pub glob: Option<&'a str>,
    pub ignore_case: bool,
    pub literal: bool,
    pub context: Option<usize>,
    pub limit: Option<usize>,
}

pub fn grep(pattern: &str, path: &Path, options: GrepOptions<'_>) -> Result<String> {
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut command = Command::new("rg");
    command
        .arg("--line-number")
        .arg("--color")
        .arg("never")
        .arg("--hidden")
        .arg("--glob")
        .arg("!.git/**")
        .arg("--glob")
        .arg("!target/**")
        .arg("--glob")
        .arg("!node_modules/**")
        .arg("--max-count")
        .arg(limit.to_string());
    if let Some(glob) = options.glob.filter(|glob| !glob.trim().is_empty()) {
        command.arg("--glob").arg(glob);
    }
    if options.ignore_case {
        command.arg("--ignore-case");
    }
    if options.literal {
        command.arg("--fixed-strings");
    }
    if let Some(context) = options.context.filter(|context| *context > 0) {
        command.arg("--context").arg(context.to_string());
    }
    command.arg("--").arg(pattern).arg(path);
    let output = command.output();

    match output {
        Ok(output) => {
            if output.status.success() || output.status.code() == Some(1) {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    Ok("no matches".to_string())
                } else {
                    Ok(format_grep_output(&stdout, limit))
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("rg failed: {}", stderr.trim())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            grep_fallback(pattern, path, options)
        }
        Err(error) => Err(error).context("failed to run rg"),
    }
}

fn grep_fallback(pattern: &str, path: &Path, options: GrepOptions<'_>) -> Result<String> {
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matcher = (!options.literal)
        .then(|| {
            RegexBuilder::new(pattern)
                .case_insensitive(options.ignore_case)
                .build()
                .with_context(|| format!("invalid grep pattern: {pattern}"))
        })
        .transpose()?;
    let globset = build_globset(options.glob)?;
    let mut matches = FallbackMatches::default();
    visit(
        path,
        pattern,
        matcher.as_ref(),
        &globset,
        options,
        limit,
        &mut matches,
    )?;
    if matches.lines.is_empty() {
        return Ok("no matches".to_string());
    }
    Ok(format_grep_output(&matches.lines.join("\n"), limit))
}

#[derive(Default)]
struct FallbackMatches {
    lines: Vec<String>,
    real_match_count: usize,
}

fn visit(
    path: &Path,
    pattern: &str,
    matcher: Option<&regex::Regex>,
    globset: &Option<GlobSet>,
    options: GrepOptions<'_>,
    limit: usize,
    matches: &mut FallbackMatches,
) -> Result<()> {
    if path.is_dir() {
        let mut walker = WalkBuilder::new(path);
        walker.hidden(false).require_git(false);
        walker.filter_entry(|entry| {
            !entry
                .file_name()
                .to_str()
                .is_some_and(should_skip_dir_entry)
        });
        for entry in walker.build() {
            if matches.real_match_count >= limit {
                break;
            }
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_file() {
                let match_path = entry_path.strip_prefix(path).unwrap_or(entry_path);
                visit_file(
                    entry_path, match_path, pattern, matcher, globset, options, limit, matches,
                )?;
            }
        }
        return Ok(());
    }

    visit_file(
        path, path, pattern, matcher, globset, options, limit, matches,
    )
}

#[allow(clippy::too_many_arguments)]
fn visit_file(
    path: &Path,
    match_path: &Path,
    pattern: &str,
    matcher: Option<&regex::Regex>,
    globset: &Option<GlobSet>,
    options: GrepOptions<'_>,
    limit: usize,
    matches: &mut FallbackMatches,
) -> Result<()> {
    if matches.real_match_count >= limit {
        return Ok(());
    }
    if let Some(globset) = globset
        && !globset.is_match(match_path)
        && !match_path
            .file_name()
            .is_some_and(|name| globset.is_match(name))
    {
        return Ok(());
    }

    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let haystack = if options.ignore_case {
        text.to_lowercase()
    } else {
        text.clone()
    };
    let needle = if options.ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let lines = text.lines().collect::<Vec<_>>();
    let search_lines = haystack.lines().collect::<Vec<_>>();
    for index in 0..lines.len() {
        if matches.real_match_count >= limit {
            break;
        }
        let search_line = search_lines.get(index).copied().unwrap_or("");
        let matched = if options.literal {
            search_line.contains(&needle)
        } else {
            matcher.is_some_and(|matcher| matcher.is_match(lines.get(index).copied().unwrap_or("")))
        };
        if matched {
            matches.real_match_count += 1;
            push_fallback_match(path, &lines, index, options.context.unwrap_or(0), matches);
        }
    }
    Ok(())
}

fn build_globset(glob: Option<&str>) -> Result<Option<GlobSet>> {
    let Some(glob) = glob.filter(|glob| !glob.trim().is_empty()) else {
        return Ok(None);
    };
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(glob)?);
    Ok(Some(builder.build()?))
}

fn push_fallback_match(
    path: &Path,
    lines: &[&str],
    match_index: usize,
    context: usize,
    matches: &mut FallbackMatches,
) {
    let start = match_index.saturating_sub(context);
    let end = (match_index + context + 1).min(lines.len());
    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        let separator = if index == match_index { ':' } else { '-' };
        let rendered = format!(
            "{}{}{}:{}",
            path.display(),
            separator,
            index + 1,
            truncate_line(line)
        );
        if !matches.lines.contains(&rendered) {
            matches.lines.push(rendered);
        }
    }
}

fn format_grep_output(output: &str, limit: usize) -> String {
    let mut lines = output.lines().map(truncate_line).collect::<Vec<_>>();
    let limit_reached = count_match_lines(&lines) >= limit;
    let truncated_by_bytes = lines.join("\n").len() > MAX_OUTPUT_BYTES;
    let mut rendered = truncate_tail(&lines.join("\n"));
    if limit_reached {
        rendered.push_str(&format!(
            "\n\n[{limit} matches limit reached. Use limit={} for more, or refine pattern]",
            limit.saturating_mul(2).min(MAX_LIMIT)
        ));
    } else if truncated_by_bytes {
        rendered.push_str(&format!("\n\n[truncated to last {MAX_OUTPUT_BYTES} bytes]"));
    }
    lines.clear();
    rendered
}

fn count_match_lines(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| line.contains(':') && !line.starts_with("--"))
        .count()
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut truncated = line.chars().take(MAX_LINE_CHARS).collect::<String>();
    truncated.push_str(" [line truncated]");
    truncated
}

fn should_skip_dir_entry(file_name: &str) -> bool {
    matches!(file_name, ".git" | "target" | "node_modules")
}

fn truncate_tail(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    let tail = truncate_tail_to_max_bytes(output, MAX_OUTPUT_BYTES);
    let tail = tail
        .split_once('\n')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(tail);
    format!("[truncated to last {} bytes]\n{}", MAX_OUTPUT_BYTES, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_context_finds_neighboring_line() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("app.log");
        std::fs::write(&file, "cause line\nCRITICAL_FAILURE paste failed\n").unwrap();

        let output = grep(
            "CRITICAL_FAILURE",
            temp.path(),
            GrepOptions {
                context: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(output.contains("cause line"));
        assert!(output.contains("CRITICAL_FAILURE paste failed"));
    }

    #[test]
    fn fallback_does_not_return_gitignored_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(temp.path().join("ignored.txt"), "needle\n").unwrap();
        std::fs::write(temp.path().join("kept.txt"), "needle\n").unwrap();

        let output = grep_fallback("needle", temp.path(), GrepOptions::default()).unwrap();

        assert!(output.contains("kept.txt"));
        assert!(!output.contains("ignored.txt"));
    }

    #[test]
    fn fallback_glob_src_star_rs_matches_src_main_rs() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "needle\n").unwrap();
        std::fs::write(temp.path().join("main.rs"), "needle\n").unwrap();

        let output = grep_fallback(
            "needle",
            temp.path(),
            GrepOptions {
                glob: Some("src/*.rs"),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(output.contains("src/main.rs"));
        assert!(!output.contains(&format!("{}:1", temp.path().join("main.rs").display())));
    }

    #[test]
    fn fallback_glob_src_double_star_rs_matches_nested_rs() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("src/a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("lib.rs"), "needle\n").unwrap();

        let output = grep_fallback(
            "needle",
            temp.path(),
            GrepOptions {
                glob: Some("src/**/*.rs"),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(output.contains("lib.rs"));
    }

    #[test]
    fn fallback_glob_star_md_matches_readme() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("README.md"), "needle\n").unwrap();
        std::fs::write(temp.path().join("main.rs"), "needle\n").unwrap();

        let output = grep_fallback(
            "needle",
            temp.path(),
            GrepOptions {
                glob: Some("*.md"),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(output.contains("README.md"));
        assert!(!output.contains("main.rs"));
    }

    #[test]
    fn fallback_context_limit_two_returns_two_real_matches_even_with_context_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("app.log");
        std::fs::write(
            &file,
            "before one\nneedle one\nafter one\nbefore two\nneedle two\nafter two\nbefore three\nneedle three\nafter three\n",
        )
        .unwrap();

        let output = grep_fallback(
            "needle",
            temp.path(),
            GrepOptions {
                context: Some(1),
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(output.contains("needle one"));
        assert!(output.contains("needle two"));
        assert!(!output.contains("needle three"));
    }
}
