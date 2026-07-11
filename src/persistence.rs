use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, Read, Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};

pub const MAX_JSONL_RECORD_BYTES: usize = 16 * 1024 * 1024;

pub struct ExclusiveFileLock {
    _file: File,
}

impl ExclusiveFileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to open lock file {}", path.display()))?;
        flock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = unlock(&self._file);
    }
}

pub struct FileLockGuard {
    fd: libc::c_int,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.fd, libc::LOCK_UN);
        }
    }
}

pub fn lock_shared(file: &File) -> Result<FileLockGuard> {
    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_SH) };
    if result == 0 {
        Ok(FileLockGuard { fd })
    } else {
        Err(io::Error::last_os_error()).context("failed to acquire shared file lock")
    }
}

pub fn lock_exclusive(file: &File) -> Result<FileLockGuard> {
    let fd = file.as_raw_fd();
    flock_exclusive(file)?;
    Ok(FileLockGuard { fd })
}

fn flock_exclusive(file: &File) -> Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error()).context("failed to acquire file lock")
    }
}

fn unlock(file: &File) -> io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn repair_incomplete_tail(file: &mut File) -> Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(false);
    }

    let mut position = len;
    let mut chunk = [0u8; 8192];
    let truncate_to = loop {
        let read_len = position.min(chunk.len() as u64) as usize;
        position -= read_len as u64;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut chunk[..read_len])?;
        if let Some(index) = chunk[..read_len].iter().rposition(|byte| *byte == b'\n') {
            break position + index as u64 + 1;
        }
        if position == 0 {
            break 0;
        }
    };
    file.set_len(truncate_to)?;
    file.seek(SeekFrom::End(0))?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedLine {
    Eof,
    Line { terminated: bool },
    TooLong,
}

pub fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    output: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<BoundedLine> {
    output.clear();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if output.is_empty() && !too_long {
                Ok(BoundedLine::Eof)
            } else if too_long {
                Ok(BoundedLine::TooLong)
            } else {
                Ok(BoundedLine::Line { terminated: false })
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if !too_long {
            let content_len = take - usize::from(newline.is_some());
            if output.len().saturating_add(content_len) > max_bytes {
                output.clear();
                too_long = true;
            } else {
                output.extend_from_slice(&available[..content_len]);
            }
        }
        reader.consume(take);
        if newline.is_some() {
            if too_long {
                return Ok(BoundedLine::TooLong);
            }
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            return Ok(BoundedLine::Line { terminated: true });
        }
    }
}

pub fn ensure_record_size(bytes: &[u8], kind: &str) -> Result<()> {
    if bytes.len() > MAX_JSONL_RECORD_BYTES {
        anyhow::bail!(
            "{kind} record is too large: {} bytes > {} bytes",
            bytes.len(),
            MAX_JSONL_RECORD_BYTES
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Write};

    #[test]
    fn bounded_line_reader_drains_oversized_lines() {
        let input = format!("{}\nvalid\n", "x".repeat(32));
        let mut reader = BufReader::new(input.as_bytes());
        let mut output = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut output, 8).unwrap(),
            BoundedLine::TooLong
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut output, 8).unwrap(),
            BoundedLine::Line { terminated: true }
        );
        assert_eq!(output, b"valid");
    }

    #[test]
    fn incomplete_tail_repair_keeps_complete_records() {
        let temp = tempfile::tempfile().unwrap();
        let mut file = temp;
        file.write_all(b"one\ntwo\npartial").unwrap();

        assert!(repair_incomplete_tail(&mut file).unwrap());
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "one\ntwo\n");
    }
}
