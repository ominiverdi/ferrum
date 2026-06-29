use anyhow::{Context, Result};
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::task;
use uuid::Uuid;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const TERMINATE_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub struct BashOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub async fn run(command: &str, cwd: &Path, timeout: Duration) -> Result<BashOutput> {
    run_with_cancel(command, cwd, timeout, None).await
}

pub async fn run_with_cancel(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<BashOutput> {
    let command = command.to_string();
    let cwd = cwd.to_path_buf();
    task::spawn_blocking(move || run_blocking(&command, &cwd, timeout, cancel))
        .await
        .context("bash worker panicked")?
}

fn run_blocking(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<BashOutput> {
    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("failed to spawn bash command")?;

    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = spawn_reader(stdout);
    let stderr_reader = spawn_reader(stderr);
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for bash command")?
        {
            let stdout = join_reader(stdout_reader)?;
            let stderr = join_reader(stderr_reader)?;
            return Ok(BashOutput {
                status: status.code(),
                stdout: truncate_tail("stdout", &String::from_utf8_lossy(&stdout))?,
                stderr: truncate_tail("stderr", &String::from_utf8_lossy(&stderr))?,
                timed_out: false,
            });
        }

        if cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            return finish_interrupted_child(pid, child, stdout_reader, stderr_reader, "aborted");
        }

        if Instant::now() >= deadline {
            return finish_interrupted_child(
                pid,
                child,
                stdout_reader,
                stderr_reader,
                &format!(
                    "command timed out after {}s; killed process group {pid}",
                    timeout.as_secs()
                ),
            );
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn finish_interrupted_child(
    pid: u32,
    mut child: std::process::Child,
    stdout_reader: thread::JoinHandle<Result<Vec<u8>>>,
    stderr_reader: thread::JoinHandle<Result<Vec<u8>>>,
    message: &str,
) -> Result<BashOutput> {
    terminate_process_group(pid);
    let grace_deadline = Instant::now() + TERMINATE_GRACE;
    while Instant::now() < grace_deadline {
        if child
            .try_wait()
            .context("failed to wait for interrupted bash command")?
            .is_some()
        {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    kill_process_group(pid);
    let _ = child.wait();
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    let mut rendered_stderr = String::from_utf8_lossy(&stderr).into_owned();
    if !rendered_stderr.is_empty() && !rendered_stderr.ends_with('\n') {
        rendered_stderr.push('\n');
    }
    rendered_stderr.push_str(message);
    Ok(BashOutput {
        status: None,
        stdout: truncate_tail("stdout", &String::from_utf8_lossy(&stdout))?,
        stderr: truncate_tail("stderr", &rendered_stderr)?,
        timed_out: !message.eq("aborted"),
    })
}

fn spawn_reader(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut output)
                .context("failed to read bash output")?;
        }
        Ok(output)
    })
}

fn join_reader(handle: thread::JoinHandle<Result<Vec<u8>>>) -> Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("bash output reader panicked"))?
}

fn terminate_process_group(pid: u32) {
    signal_process_group(pid, libc::SIGTERM);
}

fn kill_process_group(pid: u32) {
    signal_process_group(pid, libc::SIGKILL);
}

fn signal_process_group(pid: u32, signal: libc::c_int) {
    let pgid = -(pid as libc::pid_t);
    unsafe {
        libc::kill(pgid, signal);
    }
}

fn truncate_tail(label: &str, output: &str) -> Result<String> {
    if output.len() <= MAX_OUTPUT_BYTES {
        return Ok(output.to_string());
    }
    let full_output_path =
        std::env::temp_dir().join(format!("ferrum-bash-{}-{label}.log", Uuid::new_v4()));
    std::fs::write(&full_output_path, output)
        .with_context(|| format!("failed to write {}", full_output_path.display()))?;
    let start = output.len() - MAX_OUTPUT_BYTES;
    let start = output[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .unwrap_or(start);
    Ok(format!(
        "[truncated to last {} bytes. Full output: {}]\n{}",
        MAX_OUTPUT_BYTES,
        full_output_path.display(),
        &output[start..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncation_writes_full_output_file() {
        let output = "x".repeat(MAX_OUTPUT_BYTES + 100);
        let truncated = truncate_tail("stdout", &output).unwrap();
        assert!(truncated.contains("Full output:"));
        let path = truncated
            .lines()
            .next()
            .unwrap()
            .split("Full output: ")
            .nth(1)
            .unwrap()
            .trim_end_matches(']');
        assert_eq!(std::fs::read_to_string(path).unwrap(), output);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn timeout_kills_child_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("child-finished");
        let command = format!(
            "sh -c 'sleep 3; touch {}' & wait",
            shell_quote(marker.to_string_lossy().as_ref())
        );

        let output = run(&command, temp.path(), Duration::from_millis(100))
            .await
            .unwrap();
        assert!(output.timed_out);
        assert!(output.stderr.contains("killed process group"));

        thread::sleep(Duration::from_millis(3500));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn cancellation_kills_child_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("child-finished");
        let command = format!(
            "sh -c 'sleep 3; touch {}' & wait",
            shell_quote(marker.to_string_lossy().as_ref())
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_task = Arc::clone(&cancel);
        let handle = tokio::spawn(async move {
            run_with_cancel(
                &command,
                temp.path(),
                Duration::from_secs(5),
                Some(cancel_for_task),
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.store(true, Ordering::Relaxed);
        let output = handle.await.unwrap();
        assert!(!output.timed_out);
        assert!(output.stderr.contains("aborted"));
    }

    #[tokio::test]
    async fn bash_stdin_is_closed() {
        let temp = tempfile::tempdir().unwrap();
        let output = run("cat", temp.path(), Duration::from_secs(1))
            .await
            .unwrap();
        assert!(!output.timed_out);
        assert_eq!(output.stdout, "");
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
