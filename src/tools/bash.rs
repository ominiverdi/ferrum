use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
use tokio::{process::Command, time};

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
                stdout: truncate_tail(&String::from_utf8_lossy(&output.stdout)),
                stderr: truncate_tail(&String::from_utf8_lossy(&output.stderr)),
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
