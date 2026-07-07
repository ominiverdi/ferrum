use anyhow::{Context, Result};
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
    let mut matches = Vec::new();
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matcher = (!options.literal)
        .then(|| {
            RegexBuilder::new(pattern)
                .case_insensitive(options.ignore_case)
                .build()
                .with_context(|| format!("invalid grep pattern: {pattern}"))
        })
        .transpose()?;
    visit(
        path,
        pattern,
        matcher.as_ref(),
        options,
        limit,
        &mut matches,
    )?;
    if matches.is_empty() {
        return Ok("no matches".to_string());
    }
    Ok(format_grep_output(&matches.join("\n"), limit))
}

fn visit(
    path: &Path,
    pattern: &str,
    matcher: Option<&regex::Regex>,
    options: GrepOptions<'_>,
    limit: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    if matches.len() >= limit {
        return Ok(());
    }
    if path.is_dir() {
        for entry in
            std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if should_skip_dir_entry(&name) {
                continue;
            }
            visit(&entry.path(), pattern, matcher, options, limit, matches)?;
        }
        return Ok(());
    }

    if let Some(glob) = options.glob {
        let glob = glob.trim_start_matches("**/");
        if !glob.is_empty()
            && !path
                .to_string_lossy()
                .ends_with(glob.trim_start_matches('*'))
        {
            return Ok(());
        }
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
        let search_line = search_lines.get(index).copied().unwrap_or("");
        let matched = if options.literal {
            search_line.contains(&needle)
        } else {
            matcher.is_some_and(|matcher| matcher.is_match(lines.get(index).copied().unwrap_or("")))
        };
        if matched {
            push_fallback_match(path, &lines, index, options.context.unwrap_or(0), matches);
            if count_match_lines(matches) >= limit {
                break;
            }
        }
    }
    Ok(())
}

fn push_fallback_match(
    path: &Path,
    lines: &[&str],
    match_index: usize,
    context: usize,
    matches: &mut Vec<String>,
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
        if !matches.contains(&rendered) {
            matches.push(rendered);
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
    let start = output.len() - MAX_OUTPUT_BYTES;
    let start = output[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .unwrap_or(start);
    format!(
        "[truncated to last {} bytes]\n{}",
        MAX_OUTPUT_BYTES,
        &output[start..]
    )
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
}
