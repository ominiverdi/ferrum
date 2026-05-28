use anyhow::{Context, Result};
use std::{path::Path, process::Command};

const MAX_OUTPUT_BYTES: usize = 50 * 1024;

pub fn grep(pattern: &str, path: &Path) -> Result<String> {
    let output = Command::new("rg")
        .arg("--line-number")
        .arg("--color")
        .arg("never")
        .arg(pattern)
        .arg(path)
        .output();

    match output {
        Ok(output) => {
            if output.status.success() || output.status.code() == Some(1) {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    Ok("no matches".to_string())
                } else {
                    Ok(truncate_tail(&stdout))
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("rg failed: {}", stderr.trim())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => grep_fallback(pattern, path),
        Err(error) => Err(error).context("failed to run rg"),
    }
}

fn grep_fallback(pattern: &str, path: &Path) -> Result<String> {
    let mut matches = Vec::new();
    visit(path, pattern, &mut matches)?;
    if matches.is_empty() {
        return Ok("no matches".to_string());
    }
    Ok(truncate_tail(&matches.join("\n")))
}

fn visit(path: &Path, pattern: &str, matches: &mut Vec<String>) -> Result<()> {
    if path.is_dir() {
        for entry in
            std::fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || name == "target" {
                continue;
            }
            visit(&entry.path(), pattern, matches)?;
        }
        return Ok(());
    }

    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    for (index, line) in text.lines().enumerate() {
        if line.contains(pattern) {
            matches.push(format!("{}:{}:{}", path.display(), index + 1, line));
        }
    }
    Ok(())
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
