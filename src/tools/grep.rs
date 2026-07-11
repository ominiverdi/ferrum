use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::{
    collections::{HashSet, VecDeque},
    io::Read,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const OUTPUT_MARKER_RESERVE_BYTES: usize = 160;
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 10_000;
const MAX_LINE_CHARS: usize = 2_000;
const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 8 * 1024;
const GREP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default)]
pub struct GrepOptions<'a> {
    pub glob: Option<&'a str>,
    pub ignore_case: bool,
    pub literal: bool,
    pub context: Option<usize>,
    pub limit: Option<usize>,
}

#[cfg(test)]
pub fn grep(pattern: &str, path: &Path, options: GrepOptions<'_>) -> Result<String> {
    grep_with_cancel(pattern, path, options, None)
}

pub fn grep_with_cancel(
    pattern: &str,
    path: &Path,
    options: GrepOptions<'_>,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<String> {
    let deadline = Instant::now() + GREP_TIMEOUT;
    match grep_rg(pattern, path, options, cancel, deadline) {
        Err(error) if error.downcast_ref::<RgNotFound>().is_some() => {
            grep_fallback_with_control(pattern, path, options, cancel, deadline)
        }
        result => result,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("ripgrep executable was not found")]
struct RgNotFound;

fn grep_rg(
    pattern: &str,
    path: &Path,
    options: GrepOptions<'_>,
    cancel: Option<&Arc<AtomicBool>>,
    deadline: Instant,
) -> Result<String> {
    check_control(cancel, deadline)?;
    let limit = options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut command = Command::new("rg");
    command
        .arg("--json")
        .arg("--color")
        .arg("never")
        .arg("--hidden")
        .arg("--max-columns")
        .arg(MAX_LINE_CHARS.to_string())
        .arg("--max-columns-preview")
        .arg("--glob")
        .arg("!.git/**")
        .arg("--glob")
        .arg("!target/**")
        .arg("--glob")
        .arg("!node_modules/**")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
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

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RgNotFound.into());
        }
        Err(error) => return Err(error).context("failed to run rg"),
    };
    let stdout = child.stdout.take().context("failed to capture rg stdout")?;
    let stderr = child.stderr.take().context("failed to capture rg stderr")?;
    let (sender, receiver) = mpsc::sync_channel(8);
    let stdout_handle = thread::spawn(move || stream_bounded_lines(stdout, sender));
    let stderr_handle = thread::spawn(move || read_bounded_and_drain(stderr, MAX_STDERR_BYTES));

    let mut rendered = Vec::new();
    let mut match_count = 0usize;
    let mut limit_reached = false;
    let mut byte_limit_reached = false;
    let mut cancelled = false;
    let mut timed_out = false;
    let mut stream_error = None;

    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            cancelled = true;
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(StreamMessage::Line(line)) => {
                let event: serde_json::Value = match serde_json::from_slice(&line) {
                    Ok(event) => event,
                    Err(error) => {
                        stream_error = Some(format!("rg emitted invalid JSON output: {error}"));
                        break;
                    }
                };
                let Some(kind) = event.get("type").and_then(|value| value.as_str()) else {
                    continue;
                };
                if !matches!(kind, "match" | "context") {
                    continue;
                }
                if kind == "match" {
                    match_count = match_count.saturating_add(1);
                }
                if let Some(line) = render_rg_event(&event, kind == "match")
                    && !push_output_line(&mut rendered, line)
                {
                    byte_limit_reached = true;
                    break;
                }
                if match_count >= limit {
                    limit_reached = true;
                    break;
                }
            }
            Ok(StreamMessage::TooLong) => {
                stream_error = Some(format!(
                    "rg output line exceeded {MAX_STREAM_LINE_BYTES} bytes"
                ));
                break;
            }
            Ok(StreamMessage::Error(error)) => {
                stream_error = Some(format!("failed to read rg output: {error}"));
                break;
            }
            Ok(StreamMessage::Eof) => break,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                stream_error = Some("rg output reader stopped unexpectedly".to_string());
                break;
            }
        }
    }

    drop(receiver);
    let mut completed_status = None;
    if !cancelled && !timed_out && !limit_reached && !byte_limit_reached && stream_error.is_none() {
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                cancelled = true;
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            if let Some(status) = child.try_wait().context("failed to inspect rg")? {
                completed_status = Some(status);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    if completed_status.is_none() {
        kill_process_group(&mut child);
    }
    let status = match completed_status {
        Some(status) => status,
        None => child.wait().context("failed to wait for rg")?,
    };
    let _ = stdout_handle.join();
    let stderr = stderr_handle.join().unwrap_or_default();

    if cancelled {
        anyhow::bail!("aborted");
    }
    if timed_out {
        anyhow::bail!("grep timed out after {} seconds", GREP_TIMEOUT.as_secs());
    }
    if let Some(error) = stream_error {
        anyhow::bail!(error);
    }
    if !limit_reached && !byte_limit_reached && !status.success() && status.code() != Some(1) {
        anyhow::bail!("rg failed: {}", String::from_utf8_lossy(&stderr).trim());
    }
    finish_output(rendered, limit, limit_reached, byte_limit_reached)
}

fn kill_process_group(child: &mut std::process::Child) {
    let process_group = -(child.id() as i32);
    unsafe {
        libc::kill(process_group, libc::SIGKILL);
    }
    let _ = child.kill();
}

fn render_rg_event(event: &serde_json::Value, matched: bool) -> Option<String> {
    let data = event.get("data")?;
    let path = data
        .get("path")?
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("<non-UTF-8 path>");
    let line_number = data.get("line_number")?.as_u64()?;
    let text = data
        .get("lines")?
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("<non-UTF-8 line>")
        .trim_end_matches(['\r', '\n']);
    let separator = if matched { ':' } else { '-' };
    Some(format!(
        "{path}{separator}{line_number}:{}",
        truncate_line(text)
    ))
}

#[derive(Debug)]
enum StreamMessage {
    Line(Vec<u8>),
    TooLong,
    Error(String),
    Eof,
}

fn stream_bounded_lines(mut reader: impl Read, sender: SyncSender<StreamMessage>) {
    let mut buffer = [0u8; 8192];
    let mut line = Vec::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                if !line.is_empty() {
                    let _ = sender.send(StreamMessage::Line(line));
                }
                let _ = sender.send(StreamMessage::Eof);
                return;
            }
            Ok(count) => {
                for byte in &buffer[..count] {
                    if *byte == b'\n' {
                        if sender
                            .send(StreamMessage::Line(std::mem::take(&mut line)))
                            .is_err()
                        {
                            return;
                        }
                    } else if line.len() >= MAX_STREAM_LINE_BYTES {
                        let _ = sender.send(StreamMessage::TooLong);
                        return;
                    } else {
                        line.push(*byte);
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(StreamMessage::Error(error.to_string()));
                return;
            }
        }
    }
}

fn read_bounded_and_drain(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut retained = Vec::with_capacity(limit);
    let mut buffer = [0u8; 8192];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    retained
}

#[cfg(test)]
fn grep_fallback(pattern: &str, path: &Path, options: GrepOptions<'_>) -> Result<String> {
    grep_fallback_with_control(pattern, path, options, None, Instant::now() + GREP_TIMEOUT)
}

fn grep_fallback_with_control(
    pattern: &str,
    path: &Path,
    options: GrepOptions<'_>,
    cancel: Option<&Arc<AtomicBool>>,
    deadline: Instant,
) -> Result<String> {
    check_control(cancel, deadline)?;
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
    let canonical_root = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    let mut matches = FallbackMatches::default();

    if canonical_root.is_dir() {
        let mut walker = WalkBuilder::new(&canonical_root);
        walker.hidden(false).require_git(false);
        walker.filter_entry(|entry| {
            !entry
                .file_name()
                .to_str()
                .is_some_and(should_skip_dir_entry)
        });
        for entry in walker.build() {
            check_control(cancel, deadline)?;
            if matches.real_match_count >= limit || matches.byte_limit_reached {
                break;
            }
            let entry = entry?;
            if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                if entry.path().is_file() {
                    anyhow::bail!(
                        "grep fallback refuses file symlinks: {}",
                        entry.path().display()
                    );
                }
                continue;
            }
            let entry_path = entry.path();
            if entry_path.is_file() {
                let canonical = entry_path.canonicalize().with_context(|| {
                    format!("failed to resolve grep path {}", entry_path.display())
                })?;
                if !canonical.starts_with(&canonical_root) {
                    anyhow::bail!("grep path escapes search root: {}", entry_path.display());
                }
                let match_path = entry_path
                    .strip_prefix(&canonical_root)
                    .unwrap_or(entry_path);
                visit_file(
                    entry_path,
                    match_path,
                    pattern,
                    matcher.as_ref(),
                    &globset,
                    options,
                    limit,
                    &mut matches,
                    cancel,
                    deadline,
                )?;
            }
        }
    } else {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("grep fallback refuses file symlinks: {}", path.display());
        }
        visit_file(
            &canonical_root,
            path,
            pattern,
            matcher.as_ref(),
            &globset,
            options,
            limit,
            &mut matches,
            cancel,
            deadline,
        )?;
    }

    finish_output(
        matches.lines,
        limit,
        matches.real_match_count >= limit,
        matches.byte_limit_reached,
    )
}

#[derive(Default)]
struct FallbackMatches {
    lines: Vec<String>,
    rendered_lines: HashSet<(PathBuf, usize)>,
    real_match_count: usize,
    byte_limit_reached: bool,
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
    cancel: Option<&Arc<AtomicBool>>,
    deadline: Instant,
) -> Result<()> {
    if matches.real_match_count >= limit || matches.byte_limit_reached {
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

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return Ok(()),
    };
    let mut reader = std::io::BufReader::new(file);
    let context = options.context.unwrap_or(0);
    let mut previous: VecDeque<(usize, String, bool)> =
        VecDeque::with_capacity(context.saturating_add(1));
    let mut following = 0usize;
    let mut line_number = 0usize;
    loop {
        check_control(cancel, deadline)?;
        let Some((line, line_truncated)) = read_bounded_line(&mut reader, MAX_STREAM_LINE_BYTES)?
        else {
            break;
        };
        line_number += 1;
        let line = String::from_utf8_lossy(&line)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let matched = if options.literal {
            if options.ignore_case {
                line.to_lowercase().contains(&pattern.to_lowercase())
            } else {
                line.contains(pattern)
            }
        } else {
            matcher.is_some_and(|matcher| matcher.is_match(&line))
        };

        if matched && matches.real_match_count < limit {
            for (number, prior, prior_truncated) in &previous {
                push_fallback_line(path, *number, prior, false, *prior_truncated, matches);
            }
            matches.real_match_count += 1;
            push_fallback_line(path, line_number, &line, true, line_truncated, matches);
            following = context;
        } else if following > 0 {
            push_fallback_line(path, line_number, &line, false, line_truncated, matches);
            following -= 1;
        }

        previous.push_back((line_number, line, line_truncated));
        while previous.len() > context {
            previous.pop_front();
        }
        if matches.byte_limit_reached || matches.real_match_count >= limit && following == 0 {
            break;
        }
    }
    Ok(())
}

fn read_bounded_line(
    reader: &mut impl std::io::BufRead,
    limit: usize,
) -> Result<Option<(Vec<u8>, bool)>> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut saw_data = false;
    let mut truncated = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(saw_data.then_some((retained, truncated)));
        }
        saw_data = true;
        let end = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        let ends_line = buffer[..end].ends_with(b"\n");
        let remaining = limit.saturating_sub(retained.len());
        let captured = end.min(remaining);
        retained.extend_from_slice(&buffer[..captured]);
        truncated |= captured < end;
        reader.consume(end);
        if ends_line {
            return Ok(Some((retained, truncated)));
        }
    }
}

fn push_fallback_line(
    path: &Path,
    line_number: usize,
    line: &str,
    matched: bool,
    was_truncated: bool,
    matches: &mut FallbackMatches,
) {
    if !matches
        .rendered_lines
        .insert((path.to_path_buf(), line_number))
    {
        return;
    }
    let separator = if matched { ':' } else { '-' };
    let mut text = truncate_line(line);
    if was_truncated && !text.ends_with("[line truncated]") {
        text.push_str(" [line truncated]");
    }
    let rendered = format!("{}{separator}{line_number}:{text}", path.display());
    if !push_output_line(&mut matches.lines, rendered) {
        matches.byte_limit_reached = true;
    }
}

fn build_globset(glob: Option<&str>) -> Result<Option<GlobSet>> {
    let Some(glob) = glob.filter(|glob| !glob.trim().is_empty()) else {
        return Ok(None);
    };
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(glob)?);
    Ok(Some(builder.build()?))
}

fn check_control(cancel: Option<&Arc<AtomicBool>>, deadline: Instant) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        anyhow::bail!("aborted");
    }
    if Instant::now() >= deadline {
        anyhow::bail!("grep timed out after {} seconds", GREP_TIMEOUT.as_secs());
    }
    Ok(())
}

fn push_output_line(lines: &mut Vec<String>, line: String) -> bool {
    let used = lines
        .iter()
        .map(String::len)
        .sum::<usize>()
        .saturating_add(lines.len().saturating_sub(1));
    let needed = usize::from(!lines.is_empty()).saturating_add(line.len());
    if used.saturating_add(needed) > MAX_OUTPUT_BYTES.saturating_sub(OUTPUT_MARKER_RESERVE_BYTES) {
        return false;
    }
    lines.push(line);
    true
}

fn finish_output(
    lines: Vec<String>,
    limit: usize,
    limit_reached: bool,
    byte_limit_reached: bool,
) -> Result<String> {
    if lines.is_empty() {
        return Ok("no matches".to_string());
    }
    let mut rendered = lines.join("\n");
    if limit_reached {
        rendered.push_str(&format!(
            "\n\n[{limit} matches limit reached. Use limit={} for more, or refine pattern]",
            limit.saturating_mul(2).min(MAX_LIMIT)
        ));
    } else if byte_limit_reached {
        rendered.push_str(&format!("\n\n[truncated at {MAX_OUTPUT_BYTES} bytes]"));
    }
    Ok(rendered)
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
    fn rg_limit_is_global_across_files() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "needle\nneedle\n").unwrap();
        std::fs::write(temp.path().join("b.txt"), "needle\nneedle\n").unwrap();
        let output = grep(
            "needle",
            temp.path(),
            GrepOptions {
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            output
                .lines()
                .filter(|line| line.contains(":needle"))
                .count(),
            2
        );
        assert!(output.contains("2 matches limit reached"));
    }

    #[test]
    fn rg_huge_matching_line_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("large.txt"),
            format!("needle{}\n", "x".repeat(256 * 1024)),
        )
        .unwrap();
        let output = grep("needle", temp.path(), GrepOptions::default()).unwrap();
        assert!(output.len() < MAX_OUTPUT_BYTES);
    }

    #[test]
    fn cancellation_stops_grep() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let error = grep_with_cancel("needle", temp.path(), GrepOptions::default(), Some(&cancel))
            .unwrap_err();
        assert_eq!(error.to_string(), "aborted");
    }

    #[test]
    fn fallback_deadline_stops_search() {
        let temp = tempfile::tempdir().unwrap();
        let error = grep_fallback_with_control(
            "needle",
            temp.path(),
            GrepOptions::default(),
            None,
            Instant::now(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("grep timed out"));
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

    #[cfg(unix)]
    #[test]
    fn fallback_rejects_explicit_file_symlink() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "needle\n").unwrap();
        let link = temp.path().join("link.txt");
        symlink(outside.path(), &link).unwrap();
        let error = grep_fallback("needle", &link, GrepOptions::default()).unwrap_err();
        assert!(error.to_string().contains("refuses file symlinks"));
    }

    #[cfg(unix)]
    #[test]
    fn fallback_rejects_file_symlink_inside_root() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "needle\n").unwrap();
        symlink(outside.path(), temp.path().join("link.txt")).unwrap();
        let error = grep_fallback("needle", temp.path(), GrepOptions::default()).unwrap_err();
        assert!(error.to_string().contains("refuses file symlinks"));
    }

    #[test]
    fn fallback_huge_line_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("large.txt");
        std::fs::write(
            &file,
            format!("needle{}\n", "x".repeat(MAX_STREAM_LINE_BYTES * 4)),
        )
        .unwrap();
        let output = grep_fallback("needle", &file, GrepOptions::default()).unwrap();
        assert!(output.contains("line truncated"));
        assert!(output.len() < MAX_OUTPUT_BYTES);
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
            "before one\nneedle one\nafter one\nbefore two\nneedle two\nafter two\nneedle three\n",
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
        assert!(output.contains("2 matches limit reached"));
    }
}
