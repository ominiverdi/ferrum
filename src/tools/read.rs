use crate::agent::{messages, tools::ToolOutput};
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

const MAX_BYTES: usize = 50 * 1024;
const TRUNCATED_MARKER: &str = "\n[truncated]";

pub fn read_file(path: &Path, offset: usize, limit: Option<usize>) -> Result<ToolOutput> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let header = reader
        .fill_buf()
        .with_context(|| format!("failed to read {}", path.display()))?;
    if let Ok(mime_type) = messages::detect_image_mime(&header[..header.len().min(12)]) {
        if offset != 1 || limit.is_some() {
            anyhow::bail!("offset and limit apply only to text files; omit them to view an image");
        }
        let metadata = reader.get_ref().metadata()?;
        if !metadata.is_file() {
            anyhow::bail!("image path must be a regular file: {}", path.display());
        }
        if metadata.len() > messages::MAX_IMAGE_BYTES as u64 {
            anyhow::bail!("image exceeds {} byte limit", messages::MAX_IMAGE_BYTES);
        }
        let mut data = Vec::new();
        reader
            .take((messages::MAX_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut data)?;
        let byte_length = data.len();
        let image =
            messages::image_from_bytes(mime_type.clone(), data, path.display().to_string())?;
        let messages::ContentBlock::Image { sha256, .. } = &image else {
            unreachable!("image validation returns an image block");
        };
        Ok(ToolOutput {
            content: format!(
                "Image read: {} ({mime_type}, {byte_length} bytes, sha256:{sha256}); image content attached.",
                path.display()
            ),
            images: vec![image],
        })
    } else {
        read_text_from_reader(reader, path, offset, limit).map(ToolOutput::from)
    }
}

#[cfg(test)]
pub fn read_text(path: &Path, offset: usize, limit: Option<usize>) -> Result<String> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    read_text_from_reader(BufReader::new(file), path, offset, limit)
}

fn read_text_from_reader(
    mut reader: impl BufRead,
    path: &Path,
    offset: usize,
    limit: Option<usize>,
) -> Result<String> {
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

    fn encoded_image(format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bytes, format)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn read_file_returns_real_image_content_detected_from_bytes() {
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let file = tempfile::NamedTempFile::new().unwrap();
            let bytes = encoded_image(format);
            std::fs::write(file.path(), &bytes).unwrap();
            let output = read_file(file.path(), 1, None).unwrap();
            assert!(output.content.contains(mime));
            assert_eq!(output.images.len(), 1);
            let expected = messages::image_from_path(file.path()).unwrap();
            assert_eq!(output.images[0], expected);
            if let messages::ContentBlock::Image { data_base64, .. } = &output.images[0] {
                assert!(!output.content.contains(data_base64));
            }
        }
    }

    #[test]
    fn read_file_preserves_text_behavior_even_with_image_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.png");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let output = read_file(&path, 2, Some(1)).unwrap();
        assert_eq!(output.content, "two\n");
        assert!(output.images.is_empty());
    }

    #[test]
    fn read_file_rejects_image_line_ranges_corruption_and_size_overflow() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), encoded_image(image::ImageFormat::Png)).unwrap();
        assert!(
            read_file(file.path(), 2, None)
                .unwrap_err()
                .to_string()
                .contains("only to text")
        );
        assert!(
            read_file(file.path(), 1, Some(1))
                .unwrap_err()
                .to_string()
                .contains("only to text")
        );
        std::fs::write(file.path(), b"\x89PNG\r\n\x1a\ninvalid").unwrap();
        assert!(
            read_file(file.path(), 1, None)
                .unwrap_err()
                .to_string()
                .contains("not decodable")
        );
        file.as_file()
            .set_len((messages::MAX_IMAGE_BYTES + 1) as u64)
            .unwrap();
        assert!(
            read_file(file.path(), 1, None)
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
    }

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
