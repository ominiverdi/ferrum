use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    Image {
        mime_type: String,
        data_base64: String,
        sha256: String,
        source: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn with_images(role: Role, text: impl Into<String>, images: Vec<ContentBlock>) -> Self {
        let mut content = vec![ContentBlock::Text { text: text.into() }];
        content.extend(images);
        Self { role, content }
    }

    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn display_text(&self) -> String {
        strip_think_blocks(&self.text_content())
    }
}

pub fn image_from_data_uri(data_uri: &str) -> Result<ContentBlock> {
    let rest = data_uri
        .strip_prefix("data:")
        .ok_or_else(|| anyhow::anyhow!("image data URI must start with data:"))?;
    let (header, encoded) = rest
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("image data URI missing comma"))?;
    let mime_type = header
        .strip_suffix(";base64")
        .ok_or_else(|| anyhow::anyhow!("only base64 image data URIs are supported"))?;
    if !matches!(mime_type, "image/png" | "image/jpeg" | "image/webp") {
        anyhow::bail!("unsupported image data URI type: {mime_type}");
    }
    let data = STANDARD
        .decode(encoded)
        .context("failed to decode image data URI")?;
    image_from_bytes(mime_type.to_string(), data, "pasted data URI".to_string())
}

pub fn image_from_path(path: &Path) -> Result<ContentBlock> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.len() > MAX_IMAGE_BYTES as u64 {
        anyhow::bail!(
            "image {} is too large: {} bytes > {} bytes",
            path.display(),
            metadata.len(),
            MAX_IMAGE_BYTES
        );
    }
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mime_type = detect_image_mime(&data, path)?;
    image_from_bytes(mime_type, data, path.display().to_string())
}

pub fn image_from_bytes(mime_type: String, data: Vec<u8>, source: String) -> Result<ContentBlock> {
    if data.len() > MAX_IMAGE_BYTES {
        anyhow::bail!(
            "image is too large: {} bytes > {} bytes",
            data.len(),
            MAX_IMAGE_BYTES
        );
    }
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let sha256 = format!("{:x}", hasher.finalize());
    Ok(ContentBlock::Image {
        mime_type,
        data_base64: STANDARD.encode(data),
        sha256,
        source,
    })
}

pub fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "img",
    }
}

fn detect_image_mime(data: &[u8], path: &Path) -> Result<String> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png".to_string());
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg".to_string());
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Ok("image/webp".to_string());
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if ext == "png" => Ok("image/png".to_string()),
        Some(ext) if ext == "jpg" || ext == "jpeg" => Ok("image/jpeg".to_string()),
        Some(ext) if ext == "webp" => Ok("image/webp".to_string()),
        _ => anyhow::bail!("unsupported image type: {}", path.display()),
    }
}

pub fn strip_think_blocks(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;

    loop {
        let Some(start) = rest.find("<think>") else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let after_start = &rest[start + "<think>".len()..];
        let Some(end) = after_start.find("</think>") else {
            break;
        };
        rest = &after_start[end + "</think>".len()..];
    }

    output.trim_start().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_think_blocks;

    #[test]
    fn strips_think_blocks() {
        assert_eq!(
            strip_think_blocks("<think>hidden</think>\n\nvisible"),
            "visible"
        );
        assert_eq!(strip_think_blocks("a<think>b</think>c"), "ac");
    }
}
