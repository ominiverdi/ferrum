#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
    String,
    StringEscape,
}

#[derive(Debug, Default)]
pub struct Sanitizer {
    state: State,
}

impl Sanitizer {
    pub fn push(&mut self, input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        for ch in input.chars() {
            match self.state {
                State::Text => match ch {
                    '\u{1b}' => self.state = State::Escape,
                    '\u{009b}' => self.state = State::Csi,
                    '\u{009d}' => self.state = State::Osc,
                    '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => self.state = State::String,
                    '\n' | '\r' | '\t' => output.push(ch),
                    ch if ch == '\u{7f}' || ch.is_control() => {}
                    ch => output.push(ch),
                },
                State::Escape => match ch {
                    '[' => self.state = State::Csi,
                    ']' => self.state = State::Osc,
                    'P' | 'X' | '^' | '_' => self.state = State::String,
                    '\u{1b}' => {}
                    _ => self.state = State::Text,
                },
                State::Csi => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        self.state = State::Text;
                    } else if ch == '\u{1b}' {
                        self.state = State::Escape;
                    }
                }
                State::Osc => match ch {
                    '\u{7}' | '\u{009c}' => self.state = State::Text,
                    '\u{1b}' => self.state = State::OscEscape,
                    _ => {}
                },
                State::OscEscape => match ch {
                    '\\' => self.state = State::Text,
                    '\u{7}' | '\u{009c}' => self.state = State::Text,
                    '\u{1b}' => {}
                    _ => self.state = State::Osc,
                },
                State::String => match ch {
                    '\u{009c}' => self.state = State::Text,
                    '\u{1b}' => self.state = State::StringEscape,
                    _ => {}
                },
                State::StringEscape => match ch {
                    '\\' | '\u{009c}' => self.state = State::Text,
                    '\u{1b}' => {}
                    _ => self.state = State::String,
                },
            }
        }
        output
    }
}

pub fn sanitize(input: &str) -> String {
    Sanitizer::default().push(input)
}

pub fn sanitize_title(input: &str) -> String {
    sanitize(input)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_protocols_and_controls() {
        let input = concat!(
            "safe",
            "\x1b[31mred\x1b[0m",
            "\x1b]52;c;Y2xpcA==\x07",
            "\x1bPpayload\x1b\\",
            "\u{009d}0;title\u{009c}",
            "\u{009b}31mgreen\u{009b}0m",
            "\u{0007}",
            " end\n"
        );
        assert_eq!(sanitize(input), "saferedgreen end\n");
    }

    #[test]
    fn strips_sequences_split_across_stream_chunks() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("before\x1b]52;"), "before");
        assert_eq!(sanitizer.push("c;secret\x1b"), "");
        assert_eq!(sanitizer.push("\\after"), "after");
    }

    #[test]
    fn title_is_single_line_printable_and_bounded() {
        let title = format!("a\n\u{009d}0;bad\u{009c}{}", "x".repeat(300));
        let sanitized = sanitize_title(&title);
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains("bad"));
        assert_eq!(sanitized.chars().count(), 200);
    }
}
