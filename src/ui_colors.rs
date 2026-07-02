use crate::config::ColorMode;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::IsTerminal, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorToken {
    Prompt,
    Hr,
    Assistant,
    Thinking,
    Tool,
    ToolOutput,
    Status,
    Highlight,
    Success,
    Warning,
    Error,
    DiffAdded,
    DiffRemoved,
    DiffHunk,
    DiffMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColorPalette {
    pub prompt: String,
    pub hr: String,
    pub assistant: String,
    pub thinking: String,
    pub tool: String,
    pub tool_output: String,
    pub status: String,
    pub highlight: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub diff_added: String,
    pub diff_removed: String,
    pub diff_hunk: String,
    pub diff_meta: String,
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self {
            prompt: "cyan".to_string(),
            hr: "dim".to_string(),
            assistant: "default".to_string(),
            thinking: "dim".to_string(),
            tool: "cyan".to_string(),
            tool_output: "dim".to_string(),
            status: "dim".to_string(),
            highlight: "yellow".to_string(),
            success: "green".to_string(),
            warning: "yellow".to_string(),
            error: "red".to_string(),
            diff_added: "green".to_string(),
            diff_removed: "red".to_string(),
            diff_hunk: "cyan".to_string(),
            diff_meta: "dim".to_string(),
        }
    }
}

impl ColorPalette {
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join("colors.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: BTreeMap<String, toml::Value> =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Self::from_entries(raw.into_iter().map(|(key, value)| {
            let value = match value {
                toml::Value::String(value) => Some(value),
                toml::Value::Integer(value) if (0..=255).contains(&value) => {
                    Some(value.to_string())
                }
                _ => None,
            };
            (key, value)
        })))
    }

    fn from_entries(entries: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        let mut palette = Self::default();
        for (key, value) in entries {
            let Some(value) = value else {
                eprintln!("[colors] ignoring {key}: expected string or 0-255 integer");
                continue;
            };
            if AnsiStyle::parse(&value).is_none() {
                eprintln!("[colors] ignoring {key}: unsupported color spec '{value}'");
                continue;
            }
            match key.as_str() {
                "prompt" => palette.prompt = value,
                "hr" | "separator" | "rule" => palette.hr = value,
                "assistant" | "assistant_text" => palette.assistant = value,
                "thinking" | "thinking_text" => palette.thinking = value,
                "tool" | "tool_title" => palette.tool = value,
                "tool_output" | "result" => palette.tool_output = value,
                "status" | "notice" => palette.status = value,
                "highlight" => palette.highlight = value,
                "success" => palette.success = value,
                "warning" => palette.warning = value,
                "error" => palette.error = value,
                "diff_added" | "diff_insert" | "diff_inserted" => palette.diff_added = value,
                "diff_removed" | "diff_delete" | "diff_deleted" => palette.diff_removed = value,
                "diff_hunk" => palette.diff_hunk = value,
                "diff_meta" => palette.diff_meta = value,
                _ => eprintln!("[colors] ignoring unknown color token '{key}'"),
            }
        }
        palette
    }

    pub fn paint(&self, token: ColorToken, mode: ColorMode, text: impl AsRef<str>) -> String {
        let text = text.as_ref();
        if !color_enabled(mode) {
            return text.to_string();
        }
        let Some(style) = AnsiStyle::parse(self.spec(token)) else {
            return text.to_string();
        };
        style.paint(text)
    }

    pub fn prefix_suffix(&self, token: ColorToken, mode: ColorMode) -> (String, &'static str) {
        if !color_enabled(mode) {
            return (String::new(), "");
        }
        let Some(style) = AnsiStyle::parse(self.spec(token)) else {
            return (String::new(), "");
        };
        style.prefix_suffix()
    }

    pub fn spec(&self, token: ColorToken) -> &str {
        match token {
            ColorToken::Prompt => &self.prompt,
            ColorToken::Hr => &self.hr,
            ColorToken::Assistant => &self.assistant,
            ColorToken::Thinking => &self.thinking,
            ColorToken::Tool => &self.tool,
            ColorToken::ToolOutput => &self.tool_output,
            ColorToken::Status => &self.status,
            ColorToken::Highlight => &self.highlight,
            ColorToken::Success => &self.success,
            ColorToken::Warning => &self.warning,
            ColorToken::Error => &self.error,
            ColorToken::DiffAdded => &self.diff_added,
            ColorToken::DiffRemoved => &self.diff_removed,
            ColorToken::DiffHunk => &self.diff_hunk,
            ColorToken::DiffMeta => &self.diff_meta,
        }
    }
}

pub fn color_enabled(mode: ColorMode) -> bool {
    match mode {
        ColorMode::Auto => std::io::stderr().is_terminal(),
        ColorMode::On => true,
        ColorMode::Off => false,
    }
}

#[derive(Debug, Clone)]
struct AnsiStyle {
    codes: Vec<String>,
}

impl AnsiStyle {
    fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() || matches!(spec, "default" | "normal" | "none" | "off") {
            return Some(Self { codes: Vec::new() });
        }
        if let Some(hex) = spec.strip_prefix('#') {
            let (r, g, b) = parse_hex_rgb(hex)?;
            return Some(Self {
                codes: vec![format!("38;2;{r};{g};{b}")],
            });
        }
        if let Ok(index) = spec.parse::<u8>() {
            return Some(Self {
                codes: vec![format!("38;5;{index}")],
            });
        }

        let normalized = spec.replace(['_', '-'], " ").to_ascii_lowercase();
        let parts = normalized.split_whitespace().collect::<Vec<_>>();
        if parts.is_empty() {
            return Some(Self { codes: Vec::new() });
        }

        let mut codes = Vec::new();
        let mut index = 0;
        while index < parts.len() {
            match parts[index] {
                "bold" => codes.push("1".to_string()),
                "dim" => codes.push("2".to_string()),
                "italic" => codes.push("3".to_string()),
                "underline" => codes.push("4".to_string()),
                "bright" if index + 1 < parts.len() => {
                    codes.push(named_color_code(parts[index + 1], true)?.to_string());
                    index += 1;
                }
                color => codes.push(named_color_code(color, false)?.to_string()),
            }
            index += 1;
        }
        Some(Self { codes })
    }

    fn paint(&self, text: &str) -> String {
        if self.codes.is_empty() {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", self.codes.join(";"))
    }

    fn prefix_suffix(&self) -> (String, &'static str) {
        if self.codes.is_empty() {
            return (String::new(), "");
        }
        (format!("\x1b[{}m", self.codes.join(";")), "\x1b[0m")
    }
}

fn named_color_code(color: &str, bright: bool) -> Option<u8> {
    let base = match color {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" | "purple" => 35,
        "cyan" => 36,
        "white" => 37,
        "gray" | "grey" => 90,
        _ => return None,
    };
    Some(if bright && (30..=37).contains(&base) {
        base + 60
    } else {
        base
    })
}

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_and_hex_colors() {
        assert_eq!(
            AnsiStyle::parse("bold bright-blue").unwrap().codes,
            vec!["1", "94"]
        );
        assert_eq!(
            AnsiStyle::parse("#ffaa00").unwrap().codes,
            vec!["38;2;255;170;0"]
        );
        assert_eq!(AnsiStyle::parse("245").unwrap().codes, vec!["38;5;245"]);
        assert!(AnsiStyle::parse("not-a-color").is_none());
    }

    #[test]
    fn loads_partial_palette_entries() {
        let palette = ColorPalette::from_entries([
            ("prompt".to_string(), Some("magenta".to_string())),
            ("separator".to_string(), Some("245".to_string())),
            ("unknown".to_string(), Some("red".to_string())),
            ("error".to_string(), Some("bogus".to_string())),
        ]);
        assert_eq!(palette.prompt, "magenta");
        assert_eq!(palette.hr, "245");
        assert_eq!(palette.error, ColorPalette::default().error);
    }
}
