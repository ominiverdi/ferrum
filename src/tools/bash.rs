use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::{process::Command, time};
use uuid::Uuid;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Debug)]
pub struct BashOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub async fn run(command: &str, cwd: &Path, timeout: Duration) -> Result<BashOutput> {
    let child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .output();

    match time::timeout(timeout, child).await {
        Ok(output) => {
            let output = output.context("failed to run bash command")?;
            Ok(BashOutput {
                status: output.status.code(),
                stdout: truncate_tail("stdout", &String::from_utf8_lossy(&output.stdout))?,
                stderr: truncate_tail("stderr", &String::from_utf8_lossy(&output.stderr))?,
                timed_out: false,
            })
        }
        Err(_) => Ok(BashOutput {
            status: None,
            stdout: String::new(),
            stderr: format!("command timed out after {}s", timeout.as_secs()),
            timed_out: true,
        }),
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
}
