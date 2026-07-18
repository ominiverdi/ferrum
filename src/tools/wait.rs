use crate::tools::bash;
use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::time;

const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const MAX_OUTPUT_MATCH_CHARS: usize = 1_024;
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UntilInput {
    output_matches: String,
}

#[derive(Debug)]
pub struct UntilCondition {
    matcher: Regex,
}

impl UntilCondition {
    fn matches(&self, output: &bash::BashOutput) -> bool {
        self.matcher.is_match(&output.stdout) || self.matcher.is_match(&output.stderr)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionReason {
    CheckCompleted,
    ConditionMatched,
    CommandFailed,
    MaxWaitReached,
}

impl CompletionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckCompleted => "check_completed",
            Self::ConditionMatched => "condition_matched",
            Self::CommandFailed => "command_failed",
            Self::MaxWaitReached => "max_wait_reached",
        }
    }
}

#[derive(Debug)]
pub struct WaitOutput {
    pub output: bash::BashOutput,
    pub reason: CompletionReason,
    pub checks: u64,
    pub elapsed: Duration,
}

pub struct RunOptions<'a> {
    pub command: &'a str,
    pub cwd: &'a Path,
    pub interval: Duration,
    pub timeout: Duration,
    pub until: Option<&'a UntilCondition>,
    pub max_wait: Option<Duration>,
}

pub fn parse_until(value: Option<&serde_json::Value>) -> Result<Option<UntilCondition>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let input: UntilInput = serde_json::from_value(value.clone())
        .context("wait until must contain only output_matches")?;
    let pattern_chars = input.output_matches.chars().count();
    if pattern_chars == 0 {
        anyhow::bail!("wait until.output_matches must not be empty");
    }
    if pattern_chars > MAX_OUTPUT_MATCH_CHARS {
        anyhow::bail!(
            "wait until.output_matches must be <= {MAX_OUTPUT_MATCH_CHARS} characters, got {pattern_chars}"
        );
    }
    let matcher = RegexBuilder::new(&input.output_matches)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .with_context(|| {
            format!(
                "invalid wait until.output_matches regex: {}",
                input.output_matches
            )
        })?;
    Ok(Some(UntilCondition { matcher }))
}

pub async fn run(
    options: RunOptions<'_>,
    cancel: Option<Arc<AtomicBool>>,
    progress: bool,
) -> Result<WaitOutput> {
    let RunOptions {
        command,
        cwd,
        interval,
        timeout,
        until,
        max_wait,
    } = options;
    let started = Instant::now();
    let mut checks = 0u64;

    loop {
        let delay = max_wait
            .map(|limit| interval.min(limit.saturating_sub(started.elapsed())))
            .unwrap_or(interval);
        sleep_with_progress(
            delay,
            interval,
            max_wait,
            started,
            checks + 1,
            cancel.as_ref(),
            progress,
        )
        .await?;

        checks += 1;
        let output = bash::run_with_cancel(command, cwd, timeout, cancel.clone()).await?;
        if output.outcome == bash::CommandOutcome::Cancelled {
            anyhow::bail!("aborted");
        }
        let elapsed = started.elapsed();
        let reason = if !command_succeeded(&output) {
            CompletionReason::CommandFailed
        } else if until.is_none() {
            CompletionReason::CheckCompleted
        } else if until.is_some_and(|condition| condition.matches(&output)) {
            CompletionReason::ConditionMatched
        } else if max_wait.is_some_and(|limit| elapsed >= limit) {
            CompletionReason::MaxWaitReached
        } else {
            continue;
        };

        if progress {
            eprint!("\r\n");
        }
        return Ok(WaitOutput {
            output,
            reason,
            checks,
            elapsed,
        });
    }
}

fn command_succeeded(output: &bash::BashOutput) -> bool {
    output.outcome == bash::CommandOutcome::Exited
        && output.status == Some(0)
        && !output.output_incomplete
        && output.output_error.is_none()
        && output.termination_error.is_none()
        && output.residual_descendants != Some(true)
}

#[allow(clippy::too_many_arguments)]
async fn sleep_with_progress(
    delay: Duration,
    interval: Duration,
    max_wait: Option<Duration>,
    started: Instant,
    next_check: u64,
    cancel: Option<&Arc<AtomicBool>>,
    progress: bool,
) -> Result<()> {
    let mut elapsed = Duration::ZERO;
    while elapsed < delay {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            anyhow::bail!("aborted");
        }
        if progress {
            render_progress(
                elapsed,
                delay,
                interval,
                max_wait,
                started.elapsed(),
                next_check,
            );
        }
        let step = PROGRESS_INTERVAL.min(delay - elapsed);
        time::sleep(step).await;
        elapsed += step;
    }
    if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
        anyhow::bail!("aborted");
    }
    if progress {
        render_progress(
            delay,
            delay,
            interval,
            max_wait,
            started.elapsed(),
            next_check,
        );
    }
    Ok(())
}

fn render_progress(
    delay_elapsed: Duration,
    delay: Duration,
    interval: Duration,
    max_wait: Option<Duration>,
    total_elapsed: Duration,
    next_check: u64,
) {
    let next_in = delay.saturating_sub(delay_elapsed).as_secs();
    if let Some(max_wait) = max_wait {
        let total = max_wait.as_secs();
        let done = total_elapsed.as_secs().min(total);
        eprint!(
            "\r[wait] check {next_check} in {next_in}s; {done}/{total}s elapsed; Esc or Ctrl-C aborts"
        );
    } else {
        let total = interval.as_secs();
        let done = delay_elapsed.as_secs().min(total);
        let remaining = total.saturating_sub(done);
        eprint!("\r[wait] {done}/{total}s elapsed, {remaining}s remaining; Esc or Ctrl-C aborts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str, stderr: &str) -> bash::BashOutput {
        bash::BashOutput {
            status: Some(0),
            outcome: bash::CommandOutcome::Exited,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            output_incomplete: false,
            output_error: None,
            termination_error: None,
            containment: bash::CommandContainment::ProcessGroup,
            containment_error: None,
            residual_descendants: Some(false),
        }
    }

    #[test]
    fn until_matches_stdout_or_stderr() {
        let condition = parse_until(Some(&serde_json::json!({
            "output_matches": "(?i)success|error"
        })))
        .unwrap()
        .unwrap();
        assert!(condition.matches(&output("SUCCESS", "")));
        assert!(condition.matches(&output("", "error")));
        assert!(!condition.matches(&output("running", "")));
    }

    #[test]
    fn until_rejects_invalid_or_unknown_input() {
        assert!(
            parse_until(Some(&serde_json::json!({ "output_matches": "[" })))
                .unwrap_err()
                .to_string()
                .contains("invalid wait until.output_matches regex")
        );
        assert!(
            parse_until(Some(&serde_json::json!({
                "output_matches": "done",
                "other": true
            })))
            .unwrap_err()
            .to_string()
            .contains("wait until must contain only output_matches")
        );
    }

    #[tokio::test]
    async fn repeated_wait_returns_only_after_match() {
        let temp = tempfile::tempdir().unwrap();
        let condition = parse_until(Some(&serde_json::json!({
            "output_matches": "SUCCESS"
        })))
        .unwrap()
        .unwrap();
        let command = "if [ -f ready ]; then printf SUCCESS; else touch ready; printf running; fi";

        let result = run(
            RunOptions {
                command,
                cwd: temp.path(),
                interval: Duration::from_millis(10),
                timeout: Duration::from_secs(5),
                until: Some(&condition),
                max_wait: Some(Duration::from_secs(1)),
            },
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.reason, CompletionReason::ConditionMatched);
        assert_eq!(result.checks, 2);
        assert_eq!(result.output.stdout, "SUCCESS");
    }

    #[tokio::test]
    async fn repeated_wait_returns_immediately_on_command_failure() {
        let temp = tempfile::tempdir().unwrap();
        let condition = parse_until(Some(&serde_json::json!({
            "output_matches": "SUCCESS"
        })))
        .unwrap()
        .unwrap();

        let result = run(
            RunOptions {
                command: "printf failed >&2; exit 7",
                cwd: temp.path(),
                interval: Duration::from_millis(10),
                timeout: Duration::from_secs(5),
                until: Some(&condition),
                max_wait: Some(Duration::from_secs(1)),
            },
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.reason, CompletionReason::CommandFailed);
        assert_eq!(result.checks, 1);
        assert_eq!(result.output.status, Some(7));
    }

    #[tokio::test]
    async fn repeated_wait_runs_final_check_at_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let condition = parse_until(Some(&serde_json::json!({
            "output_matches": "SUCCESS"
        })))
        .unwrap()
        .unwrap();

        let result = run(
            RunOptions {
                command: "printf running",
                cwd: temp.path(),
                interval: Duration::from_millis(10),
                timeout: Duration::from_secs(5),
                until: Some(&condition),
                max_wait: Some(Duration::from_millis(1)),
            },
            None,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.reason, CompletionReason::MaxWaitReached);
        assert_eq!(result.checks, 1);
        assert_eq!(result.output.stdout, "running");
    }

    #[tokio::test]
    async fn repeated_wait_is_cancellable_between_checks() {
        let temp = tempfile::tempdir().unwrap();
        let condition = parse_until(Some(&serde_json::json!({
            "output_matches": "SUCCESS"
        })))
        .unwrap()
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        tokio::spawn(async move {
            time::sleep(Duration::from_millis(10)).await;
            trigger.store(true, Ordering::Release);
        });

        let error = run(
            RunOptions {
                command: "printf running",
                cwd: temp.path(),
                interval: Duration::from_millis(20),
                timeout: Duration::from_secs(5),
                until: Some(&condition),
                max_wait: Some(Duration::from_secs(2)),
            },
            Some(cancel),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "aborted");
    }
}
