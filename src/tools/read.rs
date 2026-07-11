use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

const MAX_BYTES: usize = 50 * 1024;
const TRUNCATED_MARKER: &str = "\n[truncated]";

pub fn read_text(path: &Path, offset: usize, limit: Option<usize>) -> Result<String> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let start = offset.saturating_sub(1);

    for _ in 0..start {
        if read_line_bounded(&mut reader, 0)?.is_none() {
            return Ok(String::new());
        }
    }

    let mut output = Vec::new();
    let mut emitted_lines = 0usize;
    let mut truncated = false;
    loop {
        if limit.is_some_and(|limit| emitted_lines >= limit) {
            break;
        }
        let remaining = MAX_BYTES.saturating_sub(output.len());
        if remaining == 0 {
            truncated = !reader.fill_buf()?.is_empty();
            break;
        }
        let Some(line) = read_line_bounded(&mut reader, remaining)? else {
            break;
        };
        emitted_lines += 1;
        output.extend_from_slice(&line.bytes);
        if line.truncated {
            truncated = true;
            break;
        }
    }

    while std::str::from_utf8(&output).is_err_and(|error| error.error_len().is_none()) {
        output.pop();
    }
    let mut output = String::from_utf8(output)
        .with_context(|| format!("failed to decode {} as UTF-8", path.display()))?;
    if truncated {
        output.push_str(TRUNCATED_MARKER);
    }
    Ok(output)
}

struct BoundedLine {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    capture_limit: usize,
) -> Result<Option<BoundedLine>> {
    let mut bytes = Vec::with_capacity(capture_limit.min(8 * 1024));
    let mut saw_data = false;
    let mut truncated = false;

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(saw_data.then_some(BoundedLine { bytes, truncated }));
        }
        saw_data = true;
        let end = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        let segment = &buffer[..end];
        let ends_line = segment.ends_with(b"\n");
        let remaining = capture_limit.saturating_sub(bytes.len());
        let captured = segment.len().min(remaining);
        bytes.extend_from_slice(&segment[..captured]);
        truncated |= captured < segment.len();
        reader.consume(end);
        if ends_line {
            return Ok(Some(BoundedLine { bytes, truncated }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_and_counts_leading_blank_lines() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), "\n\nthird\n").unwrap();

        assert_eq!(read_text(temp.path(), 1, Some(1)).unwrap(), "\n");
        assert_eq!(read_text(temp.path(), 1, Some(2)).unwrap(), "\n\n");
        assert_eq!(read_text(temp.path(), 2, Some(2)).unwrap(), "\nthird\n");
    }

    #[test]
    fn huge_line_is_discarded_without_exceeding_output_budget() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            format!("{}\nnext\n", "x".repeat(MAX_BYTES * 8)),
        )
        .unwrap();

        let output = read_text(temp.path(), 1, None).unwrap();

        assert!(output.ends_with(TRUNCATED_MARKER));
        assert!(output.len() <= MAX_BYTES + TRUNCATED_MARKER.len());
        assert!(!output.contains("next"));
    }
}
