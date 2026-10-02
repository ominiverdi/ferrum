use super::messages::{ContentBlock, Message, TokenUsage};
use std::collections::VecDeque;

const THINKING_MIN_REPEAT: usize = 80;
const TEXT_MIN_REPEAT: usize = 100;
const MAX_REPEAT_CHARS: usize = 2_048;
const REPEAT_COPIES: usize = 3;
const TAIL_LIMIT: usize = MAX_REPEAT_CHARS * REPEAT_COPIES;
const CHECK_STRIDE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum StreamLoop {
    #[error("repeated text in model reasoning")]
    Thinking,
    #[error("repeated text in model response")]
    Text,
}

#[derive(Debug, thiserror::Error)]
#[error("{kind}")]
pub(super) struct InterruptedResponse {
    pub(super) kind: StreamLoop,
    pub(super) usage: TokenUsage,
}

#[derive(Debug, Default)]
pub(super) struct StreamGuard {
    kind: Option<StreamLoop>,
    tail: VecDeque<char>,
    tail_nonwhite: usize,
    since_check: usize,
    fence: Option<(char, usize)>,
    line_start: bool,
    marker: Option<char>,
    marker_count: usize,
    skip_line: bool,
}

impl StreamGuard {
    pub(super) fn new() -> Self {
        Self {
            line_start: true,
            ..Self::default()
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    // Return a UTF-8 byte boundary so even a very large provider chunk stops
    // being rendered at the first detected loop. No full response is retained.
    pub(super) fn push(&mut self, kind: StreamLoop, delta: &str) -> (usize, Option<StreamLoop>) {
        if self.kind != Some(kind) {
            if let Some(hit) = self.finish() {
                return (0, Some(hit));
            }
            self.reset();
            self.kind = Some(kind);
        }
        for (offset, ch) in delta.char_indices() {
            let code = self.observe_fence(ch);
            if code {
                self.tail.clear();
                self.tail_nonwhite = 0;
                self.since_check = 0;
                continue;
            }
            if self.tail.len() == TAIL_LIMIT
                && let Some(old) = self.tail.pop_front()
                && !old.is_whitespace()
            {
                self.tail_nonwhite -= 1;
            }
            self.tail.push_back(ch);
            self.tail_nonwhite += usize::from(!ch.is_whitespace());
            self.since_check += 1;
            if self.since_check >= CHECK_STRIDE {
                self.since_check = 0;
                if self.repetition() {
                    return (offset + ch.len_utf8(), Some(kind));
                }
            }
        }
        (delta.len(), None)
    }

    pub(super) fn finish(&self) -> Option<StreamLoop> {
        self.repetition().then_some(self.kind).flatten()
    }

    pub(super) fn inspect_message(message: &Message) -> Option<StreamLoop> {
        let mut guard = Self::new();
        for block in &message.content {
            let (kind, text) = match block {
                ContentBlock::Text { text } => (StreamLoop::Text, text),
                ContentBlock::Thinking { text, .. } => (StreamLoop::Thinking, text),
                _ => {
                    if let Some(hit) = guard.finish() {
                        return Some(hit);
                    }
                    guard.reset();
                    continue;
                }
            };
            if let (_, Some(hit)) = guard.push(kind, text) {
                return Some(hit);
            }
        }
        guard.finish()
    }

    fn observe_fence(&mut self, ch: char) -> bool {
        let was_code = self.fence.is_some() || self.skip_line;
        if ch == '\n' {
            let marker_line = self.line_start && self.marker_count >= 3;
            if marker_line {
                self.apply_fence();
            }
            self.line_start = true;
            self.marker = None;
            self.marker_count = 0;
            self.skip_line = false;
            return was_code || marker_line;
        }
        if self.skip_line {
            return true;
        }
        if self.line_start {
            if matches!(ch, ' ' | '\t' | '\r') && self.marker.is_none() {
                return self.fence.is_some();
            }
            if matches!(ch, '`' | '~') && self.marker.is_none_or(|marker| marker == ch) {
                self.marker = Some(ch);
                self.marker_count += 1;
                return was_code || self.marker_count >= 3;
            }
            self.line_start = false;
            if self.marker_count >= 3 {
                self.apply_fence();
                self.skip_line = true;
                return true;
            }
        }
        self.fence.is_some()
    }

    fn apply_fence(&mut self) {
        if let Some(marker) = self.marker {
            match self.fence {
                None => self.fence = Some((marker, self.marker_count)),
                Some((open, length)) if marker == open && self.marker_count >= length => {
                    self.fence = None;
                }
                _ => {}
            }
        }
    }

    fn repetition(&self) -> bool {
        let Some(kind) = self.kind else {
            return false;
        };
        let min_repeat = match kind {
            StreamLoop::Thinking => THINKING_MIN_REPEAT,
            StreamLoop::Text => TEXT_MIN_REPEAT,
        };
        let max_repeat = MAX_REPEAT_CHARS.min(self.tail.len() / REPEAT_COPIES);
        if max_repeat < min_repeat || self.tail_nonwhite < 32 * REPEAT_COPIES {
            return false;
        }
        let reversed = self.tail.iter().rev().copied().collect::<Vec<_>>();
        let z = prefix_matches(&reversed);
        (min_repeat..=max_repeat).any(|width| {
            z[width] >= width * (REPEAT_COPIES - 1)
                && reversed[..width]
                    .iter()
                    .filter(|ch| !ch.is_whitespace())
                    .count()
                    >= 32
        })
    }
}

// A linear prefix-match table over the bounded tail avoids quadratic scans
// when the model produces a long, highly repetitive stream.
fn prefix_matches(text: &[char]) -> Vec<usize> {
    let mut matches = vec![0; text.len()];
    let (mut start, mut end) = (0, 0);
    for index in 1..text.len() {
        if index < end {
            matches[index] = matches[index - start].min(end - index);
        }
        while index + matches[index] < text.len()
            && text[matches[index]] == text[index + matches[index]]
        {
            matches[index] += 1;
        }
        if index + matches[index] > end {
            start = index;
            end = index + matches[index];
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::super::messages::Role;
    use super::*;

    fn paragraph() -> String {
        "The model is repeating the same explanation instead of making progress on the concrete task and checking a new result. ".to_string()
    }

    #[test]
    fn detects_reasoning_and_answer_repetition_across_chunks() {
        for kind in [StreamLoop::Thinking, StreamLoop::Text] {
            let mut guard = StreamGuard::new();
            let text = paragraph().repeat(5);
            let mut detected = false;
            for ch in text.chars() {
                if guard.push(kind, &ch.to_string()).1 == Some(kind) {
                    detected = true;
                    break;
                }
            }
            assert!(detected);
        }
    }

    #[test]
    fn two_copies_are_not_enough_and_final_tail_is_checked() {
        let text = paragraph();
        let mut guard = StreamGuard::new();
        assert_eq!(guard.push(StreamLoop::Text, &text.repeat(2)).1, None);
        assert_eq!(guard.finish(), None);
        let mut guard = StreamGuard::new();
        let (_, hit) = guard.push(StreamLoop::Text, &text.repeat(3));
        assert_eq!(hit.or_else(|| guard.finish()), Some(StreamLoop::Text));
    }

    #[test]
    fn stops_partway_through_a_large_chunk_at_a_utf8_boundary() {
        let mut guard = StreamGuard::new();
        let delta =
            "café 日本語 repeat this explanation without making any new progress. ".repeat(5_000);
        let (accepted, hit) = guard.push(StreamLoop::Text, &delta);
        assert_eq!(hit, Some(StreamLoop::Text));
        assert!(accepted < 1_000);
        assert!(delta.is_char_boundary(accepted));
    }

    #[test]
    fn ignores_fenced_code_even_when_fences_arrive_in_pieces() {
        for fence in ["```", "~~~"] {
            let mut guard = StreamGuard::new();
            let text = format!("{fence}rust\n{}\n{fence}\n", paragraph().repeat(20));
            for ch in text.chars() {
                assert_eq!(guard.push(StreamLoop::Text, &ch.to_string()).1, None);
            }
            assert_eq!(guard.finish(), None);
            let (_, hit) = guard.push(StreamLoop::Text, &paragraph().repeat(5));
            assert_eq!(hit, Some(StreamLoop::Text));
        }
    }

    #[test]
    fn whitespace_and_short_repetitive_code_are_not_loops() {
        for text in [
            " \n".repeat(10_000),
            "```\nlet x = 1;\n".to_string() + &"let x = 1;\n".repeat(1_000),
        ] {
            let mut guard = StreamGuard::new();
            assert_eq!(guard.push(StreamLoop::Text, &text).1, None);
            assert_eq!(guard.finish(), None);
        }
    }

    #[test]
    fn unique_prose_and_similar_paragraph_openings_are_not_loops() {
        let mut guard = StreamGuard::new();
        for i in 0..200 {
            let text = format!(
                "The result for this file follows the same report format, but the actual finding is distinct: file {i} has result {i}.\n\n"
            );
            assert_eq!(guard.push(StreamLoop::Text, &text).1, None);
        }
        assert_eq!(guard.finish(), None);
        assert!(guard.tail.len() <= TAIL_LIMIT);
    }

    #[test]
    fn does_not_combine_reasoning_and_answer_or_separate_requests() {
        let text = paragraph();
        let mut guard = StreamGuard::new();
        assert_eq!(guard.push(StreamLoop::Thinking, &text.repeat(2)).1, None);
        assert_eq!(guard.push(StreamLoop::Text, &text).1, None);
        assert_eq!(guard.finish(), None);
        guard.reset();
        assert_eq!(guard.push(StreamLoop::Text, &text.repeat(2)).1, None);
        assert_eq!(guard.finish(), None);
    }

    #[test]
    fn longest_repeating_block_and_tail_are_bounded() {
        let block = (0..MAX_REPEAT_CHARS)
            .map(|i| char::from_u32(0x4e00 + i as u32).unwrap())
            .collect::<String>();
        let mut guard = StreamGuard::new();
        let text = block.repeat(3);
        let (_, hit) = guard.push(StreamLoop::Text, &text);
        assert_eq!(hit.or_else(|| guard.finish()), Some(StreamLoop::Text));
        assert_eq!(guard.tail.len(), TAIL_LIMIT);
    }

    #[test]
    fn a_shorter_marker_does_not_close_a_longer_code_fence() {
        let text = format!("````rust\n```\n{}\n````\n", paragraph().repeat(10));
        let mut guard = StreamGuard::new();
        assert_eq!(guard.push(StreamLoop::Text, &text).1, None);
        assert_eq!(guard.finish(), None);
        assert_eq!(
            guard.push(StreamLoop::Text, &paragraph().repeat(5)).1,
            Some(StreamLoop::Text)
        );
    }

    #[test]
    fn checks_buffered_responses_without_modifying_signed_reasoning() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: paragraph().repeat(5),
                signature: Some("opaque-signature".to_string()),
            }],
            usage: None,
        };
        assert_eq!(
            StreamGuard::inspect_message(&message),
            Some(StreamLoop::Thinking)
        );
        assert!(matches!(
            &message.content[0],
            ContentBlock::Thinking {
                signature: Some(_),
                ..
            }
        ));
    }
}
