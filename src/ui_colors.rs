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
        Self::load_file_lenient(&path)
    }

    pub fn load_palette_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: BTreeMap<String, toml::Value> =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        Self::from_raw_entries(raw, true)
            .with_context(|| format!("invalid palette {}", path.display()))
    }

    fn load_file_lenient(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: BTreeMap<String, toml::Value> =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        Self::from_raw_entries(raw, false)
    }

    fn from_raw_entries(raw: BTreeMap<String, toml::Value>, strict: bool) -> Result<Self> {
        Self::from_entries_impl(
            raw.into_iter().map(|(key, value)| {
                let value = match value {
                    toml::Value::String(value) => Some(value),
                    toml::Value::Integer(value) if (0..=255).contains(&value) => {
                        Some(value.to_string())
                    }
                    _ => None,
                };
                (key, value)
            }),
            strict,
        )
    }

    #[cfg(test)]
    fn from_entries(entries: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        Self::from_entries_impl(entries, false).expect("lenient palette parsing cannot fail")
    }

    fn from_entries_impl(
        entries: impl IntoIterator<Item = (String, Option<String>)>,
        strict: bool,
    ) -> Result<Self> {
        let mut palette = Self::default();
        for (key, value) in entries {
            let Some(value) = value else {
                let message = format!("{key}: expected string or 0-255 integer");
                if strict {
                    anyhow::bail!(message);
                }
                eprintln!("[colors] ignoring {message}");
                continue;
            };
            if AnsiStyle::parse(&value).is_none() {
                let message = format!("{key}: unsupported color spec '{value}'");
                if strict {
                    anyhow::bail!(message);
                }
                eprintln!("[colors] ignoring {message}");
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
                _ => {
                    let message = format!("unknown color token '{key}'");
                    if strict {
                        anyhow::bail!(message);
                    }
                    eprintln!("[colors] ignoring {message}");
                }
            }
        }
        Ok(palette)
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
        let mut color_parts = Vec::new();
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
                color => color_parts.push(color),
            }
            index += 1;
        }

        if !color_parts.is_empty() {
            if color_parts.len() == 1 {
                if let Some(code) = named_color_code(color_parts[0], false) {
                    codes.push(code.to_string());
                    return Some(Self { codes });
                }
            }
            let color_name = color_parts.join("");
            if let Some(hex) = color_name.strip_prefix('#') {
                let (r, g, b) = parse_hex_rgb(hex)?;
                codes.push(format!("38;2;{r};{g};{b}"));
                return Some(Self { codes });
            }
            if let Ok(index) = color_name.parse::<u8>() {
                codes.push(format!("38;5;{index}"));
                return Some(Self { codes });
            }
            let index = xterm_color_index(&color_name)?;
            codes.push(format!("38;5;{index}"));
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

fn normalize_xterm_color_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<String>()
        .replace("grey", "gray")
}

fn xterm_color_index(name: &str) -> Option<u8> {
    match normalize_xterm_color_name(name).as_str() {
        "black" => Some(0),
        "maroon" => Some(1),
        "green" => Some(2),
        "olive" => Some(3),
        "navy" => Some(4),
        "purple" => Some(5),
        "teal" => Some(6),
        "silver" => Some(7),
        "gray" => Some(8),
        "red" => Some(9),
        "lime" => Some(10),
        "yellow" => Some(11),
        "blue" => Some(12),
        "fuchsia" => Some(13),
        "aqua" => Some(14),
        "white" => Some(15),
        "gray0" => Some(16),
        "navyblue" => Some(17),
        "darkblue" => Some(18),
        "blue3" => Some(19),
        "blue1" => Some(21),
        "darkgreen" => Some(22),
        "deepskyblue4" => Some(23),
        "dodgerblue3" => Some(26),
        "dodgerblue2" => Some(27),
        "green4" => Some(28),
        "springgreen4" => Some(29),
        "turquoise4" => Some(30),
        "deepskyblue3" => Some(31),
        "dodgerblue1" => Some(33),
        "green3" => Some(34),
        "springgreen3" => Some(35),
        "darkcyan" => Some(36),
        "lightseagreen" => Some(37),
        "deepskyblue2" => Some(38),
        "deepskyblue1" => Some(39),
        "springgreen2" => Some(42),
        "cyan3" => Some(43),
        "darkturquoise" => Some(44),
        "turquoise2" => Some(45),
        "green1" => Some(46),
        "springgreen1" => Some(48),
        "mediumspringgreen" => Some(49),
        "cyan2" => Some(50),
        "cyan1" => Some(51),
        "darkred" => Some(52),
        "deeppink4" => Some(53),
        "purple4" => Some(54),
        "purple3" => Some(56),
        "blueviolet" => Some(57),
        "orange4" => Some(58),
        "gray37" => Some(59),
        "mediumpurple4" => Some(60),
        "slateblue3" => Some(61),
        "royalblue1" => Some(63),
        "chartreuse4" => Some(64),
        "darkseagreen4" => Some(65),
        "paleturquoise4" => Some(66),
        "steelblue" => Some(67),
        "steelblue3" => Some(68),
        "cornflowerblue" => Some(69),
        "chartreuse3" => Some(70),
        "cadetblue" => Some(72),
        "skyblue3" => Some(74),
        "steelblue1" => Some(75),
        "palegreen3" => Some(77),
        "seagreen3" => Some(78),
        "aquamarine3" => Some(79),
        "mediumturquoise" => Some(80),
        "chartreuse2" => Some(82),
        "seagreen2" => Some(83),
        "seagreen1" => Some(84),
        "aquamarine1" => Some(86),
        "darkslategray2" => Some(87),
        "darkmagenta" => Some(90),
        "darkviolet" => Some(92),
        "lightpink4" => Some(95),
        "plum4" => Some(96),
        "mediumpurple3" => Some(97),
        "slateblue1" => Some(99),
        "yellow4" => Some(100),
        "wheat4" => Some(101),
        "gray53" => Some(102),
        "lightslategray" => Some(103),
        "mediumpurple" => Some(104),
        "lightslateblue" => Some(105),
        "darkolivegreen3" => Some(107),
        "darkseagreen" => Some(108),
        "lightskyblue3" => Some(109),
        "skyblue2" => Some(111),
        "darkseagreen3" => Some(115),
        "darkslategray3" => Some(116),
        "skyblue1" => Some(117),
        "chartreuse1" => Some(118),
        "lightgreen" => Some(119),
        "palegreen1" => Some(121),
        "darkslategray1" => Some(123),
        "red3" => Some(124),
        "mediumvioletred" => Some(126),
        "magenta3" => Some(127),
        "darkorange3" => Some(130),
        "indianred" => Some(131),
        "hotpink3" => Some(132),
        "mediumorchid3" => Some(133),
        "mediumorchid" => Some(134),
        "mediumpurple2" => Some(135),
        "darkgoldenrod" => Some(136),
        "lightsalmon3" => Some(137),
        "rosybrown" => Some(138),
        "gray63" => Some(139),
        "mediumpurple1" => Some(141),
        "gold3" => Some(142),
        "darkkhaki" => Some(143),
        "navajowhite3" => Some(144),
        "gray69" => Some(145),
        "lightsteelblue3" => Some(146),
        "lightsteelblue" => Some(147),
        "yellow3" => Some(148),
        "darkseagreen2" => Some(151),
        "lightcyan3" => Some(152),
        "lightskyblue1" => Some(153),
        "greenyellow" => Some(154),
        "darkolivegreen2" => Some(155),
        "darkseagreen1" => Some(158),
        "paleturquoise1" => Some(159),
        "deeppink3" => Some(161),
        "magenta2" => Some(165),
        "hotpink2" => Some(169),
        "orchid" => Some(170),
        "mediumorchid1" => Some(171),
        "orange3" => Some(172),
        "lightpink3" => Some(174),
        "pink3" => Some(175),
        "plum3" => Some(176),
        "violet" => Some(177),
        "lightgoldenrod3" => Some(179),
        "tan" => Some(180),
        "mistyrose3" => Some(181),
        "thistle3" => Some(182),
        "plum2" => Some(183),
        "khaki3" => Some(185),
        "lightgoldenrod2" => Some(186),
        "lightyellow3" => Some(187),
        "gray84" => Some(188),
        "lightsteelblue1" => Some(189),
        "yellow2" => Some(190),
        "darkolivegreen1" => Some(191),
        "honeydew2" => Some(194),
        "lightcyan1" => Some(195),
        "red1" => Some(196),
        "deeppink2" => Some(197),
        "deeppink1" => Some(198),
        "magenta1" => Some(201),
        "orangered1" => Some(202),
        "indianred1" => Some(203),
        "hotpink" => Some(205),
        "darkorange" => Some(208),
        "salmon1" => Some(209),
        "lightcoral" => Some(210),
        "palevioletred1" => Some(211),
        "orchid2" => Some(212),
        "orchid1" => Some(213),
        "orange1" => Some(214),
        "sandybrown" => Some(215),
        "lightsalmon1" => Some(216),
        "lightpink1" => Some(217),
        "pink1" => Some(218),
        "plum1" => Some(219),
        "gold1" => Some(220),
        "navajowhite1" => Some(223),
        "mistyrose1" => Some(224),
        "thistle1" => Some(225),
        "yellow1" => Some(226),
        "lightgoldenrod1" => Some(227),
        "khaki1" => Some(228),
        "wheat1" => Some(229),
        "cornsilk1" => Some(230),
        "gray100" => Some(231),
        "gray3" => Some(232),
        "gray7" => Some(233),
        "gray11" => Some(234),
        "gray15" => Some(235),
        "gray19" => Some(236),
        "gray23" => Some(237),
        "gray27" => Some(238),
        "gray30" => Some(239),
        "gray35" => Some(240),
        "gray39" => Some(241),
        "gray42" => Some(242),
        "gray46" => Some(243),
        "gray50" => Some(244),
        "gray54" => Some(245),
        "gray58" => Some(246),
        "gray62" => Some(247),
        "gray66" => Some(248),
        "gray70" => Some(249),
        "gray74" => Some(250),
        "gray78" => Some(251),
        "gray82" => Some(252),
        "gray85" => Some(253),
        "gray89" => Some(254),
        "gray93" => Some(255),
        _ => None,
    }
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
        assert_eq!(AnsiStyle::parse("orange3").unwrap().codes, vec!["38;5;172"]);
        assert_eq!(
            AnsiStyle::parse("bold LightSkyBlue3").unwrap().codes,
            vec!["1", "38;5;109"]
        );
        assert_eq!(
            AnsiStyle::parse("light-sky-blue-3").unwrap().codes,
            vec!["38;5;109"]
        );
        assert_eq!(AnsiStyle::parse("grey93").unwrap().codes, vec!["38;5;255"]);
        assert_eq!(
            AnsiStyle::parse("bold #ffaa00").unwrap().codes,
            vec!["1", "38;2;255;170;0"]
        );
        assert_eq!(
            AnsiStyle::parse("dim 245").unwrap().codes,
            vec!["2", "38;5;245"]
        );
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

    #[test]
    fn strict_palette_loading_rejects_invalid_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bad.toml");
        fs::write(&path, "prompt = \"DeepSkyBlue1\"\nerror = \"bogus\"\n").unwrap();

        let error = ColorPalette::load_palette_file(&path).unwrap_err();
        assert!(error.to_string().contains("invalid palette"));
    }
}
