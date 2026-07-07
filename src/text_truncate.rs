pub fn truncate_to_max_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

pub fn truncate_tail_to_max_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_utf8_safely() {
        let text = format!("{}é", "x".repeat(10));
        let truncated = truncate_to_max_bytes(&text, 11);
        assert_eq!(truncated, "x".repeat(10));
        let tail = truncate_tail_to_max_bytes(&text, 1);
        assert!(tail.is_empty() || tail == "é");
    }
}
