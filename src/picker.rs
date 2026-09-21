use crate::terminal_text;
use anyhow::Result;
use crossterm::{
    cursor::{MoveToColumn, MoveToPreviousLine},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType},
};
use std::{
    io::{self, Write},
    ops::Range,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_QUERY_CHARS: usize = 256;
/// Frame lines other than the item list: leading blank, title, blank, trailing blank, prompt.
const FRAME_CHROME_LINES: usize = 5;

#[derive(Debug, Clone)]
pub(crate) struct PickerItem<T> {
    pub(crate) value: T,
    pub(crate) label: String,
    pub(crate) description: Option<String>,
    pub(crate) search_terms: Vec<String>,
    pub(crate) current: bool,
}

impl<T> PickerItem<T> {
    pub(crate) fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            description: None,
            search_terms: Vec::new(),
            current: false,
        }
    }

    pub(crate) fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub(crate) fn with_search_terms(
        mut self,
        search_terms: impl IntoIterator<Item = String>,
    ) -> Self {
        self.search_terms.extend(search_terms);
        self
    }

    pub(crate) fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

pub(crate) fn pick<T: Clone>(title: &str, items: &[PickerItem<T>]) -> Result<Option<T>> {
    if items.is_empty() {
        return Ok(None);
    }

    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let layout = FrameLayout::for_terminal(cols, rows);
    let mut raw_mode = RawModeGuard::enable()?;
    let outcome = run_picker(title, items, layout, &mut io::stdout(), event::read)?;
    raw_mode.disable()?;
    Ok(outcome)
}

fn run_picker<T: Clone>(
    title: &str,
    items: &[PickerItem<T>],
    mut layout: FrameLayout,
    mut stdout: impl Write,
    mut read_event: impl FnMut() -> io::Result<Event>,
) -> Result<Option<T>> {
    let mut query = String::new();
    let mut cursor = initial_cursor(items);
    let mut notice: Option<String> = None;

    // Land on column zero so the first frame starts on a clean line.
    stdout.write_all(b"\r")?;
    let mut drawn_lines = 0usize;

    let outcome = loop {
        // A digits-only query names a displayed number, so it must not narrow the list:
        // the numbers on screen have to keep matching the items they label.
        let number = query_number(&query);
        let filtered = filtered_indices(items, if number.is_some() { "" } else { &query });
        if let Some(number) = number {
            cursor = number.saturating_sub(1);
        }
        cursor = clamp_cursor(cursor, filtered.len());
        let frame = render_picker(
            title,
            items,
            &filtered,
            &query,
            cursor,
            notice.as_deref(),
            layout,
        );
        let frame_lines = frame.matches("\r\n").count() + 1;
        if drawn_lines > 0 {
            // Return to the top of the previous frame before clearing it.
            let up = drawn_lines - 1;
            if up > 0 {
                execute!(stdout, MoveToPreviousLine(up as u16))?;
            }
            execute!(stdout, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        }
        stdout.write_all(frame.as_bytes())?;
        stdout.flush()?;
        drawn_lines = frame_lines;
        notice = None;

        match read_picker_event(&mut read_event)? {
            PickerEvent::Resize(cols, rows) => layout = FrameLayout::for_terminal(cols, rows),
            PickerEvent::Cancel => break None,
            PickerEvent::EndOfInput => {
                if query.is_empty() {
                    break None;
                }
            }
            PickerEvent::Up => {
                leave_number_entry(&mut query);
                cursor = move_highlight_up(cursor, filtered.len());
            }
            PickerEvent::Down => {
                leave_number_entry(&mut query);
                cursor = move_highlight_down(cursor, filtered.len());
            }
            PickerEvent::Home => {
                leave_number_entry(&mut query);
                cursor = 0;
            }
            PickerEvent::End => {
                leave_number_entry(&mut query);
                cursor = filtered.len().saturating_sub(1);
            }
            PickerEvent::Backspace => {
                query.pop();
                cursor = preferred_cursor(items, &query);
            }
            PickerEvent::Char(character) => {
                if query.chars().count() < MAX_QUERY_CHARS {
                    query.push(character);
                    cursor = preferred_cursor(items, &query);
                }
            }
            PickerEvent::Paste(text) => {
                for character in text.chars().filter(|character| !character.is_control()) {
                    if query.chars().count() >= MAX_QUERY_CHARS {
                        break;
                    }
                    query.push(character);
                }
                cursor = preferred_cursor(items, &query);
            }
            PickerEvent::Enter => match enter_index(number, filtered.len(), cursor) {
                Some(index) => break Some(items[filtered[index]].value.clone()),
                None => {
                    notice = Some(match number {
                        Some(number) => format!("No selection {number}"),
                        None => "No matching selections".to_string(),
                    });
                }
            },
        }
    };

    stdout.write_all(b"\r\n")?;
    stdout.flush()?;
    Ok(outcome)
}

/// Item and column budget for a menu frame on a terminal of a given size.
#[derive(Debug, Clone, Copy)]
struct FrameLayout {
    max_items: usize,
    max_width: usize,
}

impl FrameLayout {
    fn for_terminal(cols: u16, rows: u16) -> Self {
        Self {
            max_items: picker_capacity(rows),
            // Leave the final column unused so no composed line can trigger terminal
            // auto-wrap, which would desynchronize the cursor-up redraw.
            max_width: usize::from(cols).saturating_sub(1).max(1),
        }
    }
}

/// Number of items that fit in a menu frame for a terminal `rows` lines tall.
fn picker_capacity(rows: u16) -> usize {
    usize::from(rows).saturating_sub(FRAME_CHROME_LINES).max(1)
}

/// Index of the item that best represents the current state, if any.
fn initial_cursor<T>(items: &[PickerItem<T>]) -> usize {
    items.iter().position(|item| item.current).unwrap_or(0)
}

/// Keep the highlighted item on the current value when a filter still matches it.
fn preferred_cursor<T>(items: &[PickerItem<T>], query: &str) -> usize {
    filtered_indices(items, query)
        .into_iter()
        .position(|index| items[index].current)
        .unwrap_or(0)
}

/// 1-based item number when the query is only digits, matching the numbered rows.
///
/// Such a query names a visible menu number and selects instead of filtering. Any other
/// query - including text that merely contains digits, such as `gpt-5` - filters as usual.
fn query_number(query: &str) -> Option<usize> {
    let trimmed = query.trim();
    (!trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit()))
        .then(|| trimmed.parse::<usize>().unwrap_or(usize::MAX))
}

/// Arrow keys leave number entry, so browsing the list always wins over a typed number.
fn leave_number_entry(query: &mut String) {
    if query_number(query).is_some() {
        query.clear();
    }
}

/// Index into `filtered` that Enter selects, if any.
fn enter_index(number: Option<usize>, filtered_len: usize, cursor: usize) -> Option<usize> {
    match number {
        Some(number) => number.checked_sub(1).filter(|index| *index < filtered_len),
        None => (cursor < filtered_len).then_some(cursor),
    }
}

fn clamp_cursor(cursor: usize, len: usize) -> usize {
    if len == 0 { 0 } else { cursor.min(len - 1) }
}

/// Move the highlight up by one item, wrapping at the top.
fn move_highlight_up(cursor: usize, len: usize) -> usize {
    let cursor = clamp_cursor(cursor, len);
    if len == 0 {
        0
    } else if cursor == 0 {
        len - 1
    } else {
        cursor - 1
    }
}

/// Move the highlight down by one item, wrapping at the bottom.
fn move_highlight_down(cursor: usize, len: usize) -> usize {
    let cursor = clamp_cursor(cursor, len);
    if len == 0 || cursor + 1 >= len {
        0
    } else {
        cursor + 1
    }
}

/// Slice of `0..len` to show so the frame fits `max_items` while keeping `cursor` visible.
fn visible_window(cursor: usize, len: usize, max_items: usize) -> Range<usize> {
    if max_items == 0 || len <= max_items {
        return 0..len;
    }
    let cursor = cursor.min(len - 1);
    let mut start = cursor.saturating_sub(max_items / 2);
    if start + max_items > len {
        start = len - max_items;
    }
    start..start + max_items
}

fn filtered_indices<T>(items: &[PickerItem<T>], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            (query.is_empty()
                || item.label.to_lowercase().contains(&query)
                || item
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_lowercase().contains(&query))
                || item
                    .search_terms
                    .iter()
                    .any(|term| term.to_lowercase().contains(&query)))
            .then_some(index)
        })
        .collect()
}

/// Sanitize terminal sequences and flatten to a single line so frame line counts stay exact.
fn single_line(input: &str) -> String {
    terminal_text::sanitize(input).replace(['\r', '\n', '\t'], " ")
}

/// Bound a line to terminal columns without splitting grapheme clusters.
fn truncate_line(line: &str, max_width: usize) -> String {
    if line.width() <= max_width {
        return line.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut truncated = String::new();
    let mut width = 0;
    for grapheme in line.graphemes(true) {
        let grapheme_width = grapheme.width();
        if width + grapheme_width > max_width - 3 {
            break;
        }
        truncated.push_str(grapheme);
        width += grapheme_width;
    }
    truncated.push_str("...");
    truncated
}

fn render_picker<T>(
    title: &str,
    items: &[PickerItem<T>],
    filtered: &[usize],
    query: &str,
    cursor: usize,
    notice: Option<&str>,
    layout: FrameLayout,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let title = single_line(title);
    let number = query_number(query);
    let trimmed_query = single_line(query.trim());
    lines.push(String::new());
    if trimmed_query.is_empty() || number.is_some() {
        // A typed number selects a row instead of narrowing the list, so the title stays plain.
        lines.push(title);
    } else {
        lines.push(format!("{title} matching \"{trimmed_query}\""));
    }
    lines.push(String::new());

    if filtered.is_empty() {
        lines.push("No matching selections".to_string());
    } else {
        let number_width = filtered.len().to_string().len();
        let window = visible_window(cursor, filtered.len(), layout.max_items);
        for (offset, item_index) in filtered[window.clone()].iter().enumerate() {
            let display_index = window.start + offset;
            let item = &items[*item_index];
            let marker = if display_index == cursor { '>' } else { ' ' };
            let label = single_line(&item.label);
            let current = if item.current { " (current)" } else { "" };
            let description = item
                .description
                .as_deref()
                .map(single_line)
                .filter(|description| !description.is_empty())
                .map(|description| format!(" - {description}"))
                .unwrap_or_default();
            lines.push(format!(
                "{marker} {:>number_width$}  {label}{description}{current}",
                display_index + 1
            ));
        }
    }
    lines.push(String::new());
    lines.push(prompt_line(query, notice, number, filtered.len()));
    lines
        .iter()
        .map(|line| truncate_line(line, layout.max_width))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn prompt_line(query: &str, notice: Option<&str>, number: Option<usize>, len: usize) -> String {
    if let Some(notice) = notice {
        return single_line(notice);
    }
    if let Some(number) = number {
        return match number.checked_sub(1) {
            Some(index) if index < len => {
                format!("Number {number} - Enter to select, Esc to cancel")
            }
            _ => format!("No selection {number}"),
        };
    }
    let query = single_line(query);
    if query.trim().is_empty() {
        "Type to filter or a number, Up/Down to move, Enter selects, Esc cancels".to_string()
    } else {
        format!("Filter: {query}")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PickerEvent {
    Resize(u16, u16),
    Cancel,
    EndOfInput,
    Up,
    Down,
    Home,
    End,
    Enter,
    Backspace,
    Char(char),
    Paste(String),
}

fn read_picker_event(read_event: &mut impl FnMut() -> io::Result<Event>) -> Result<PickerEvent> {
    loop {
        match read_event()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let mapped = match key.code {
                    KeyCode::Esc => PickerEvent::Cancel,
                    KeyCode::Enter => PickerEvent::Enter,
                    KeyCode::Up => PickerEvent::Up,
                    KeyCode::Down => PickerEvent::Down,
                    KeyCode::Home => PickerEvent::Home,
                    KeyCode::End => PickerEvent::End,
                    KeyCode::Backspace => PickerEvent::Backspace,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        PickerEvent::Cancel
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        PickerEvent::EndOfInput
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        PickerEvent::Char(character)
                    }
                    _ => continue,
                };
                return Ok(mapped);
            }
            Event::Paste(text) => return Ok(PickerEvent::Paste(text)),
            Event::Resize(cols, rows) => return Ok(PickerEvent::Resize(cols, rows)),
            _ => {}
        }
    }
}

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self { enabled: true })
    }

    fn disable(&mut self) -> Result<()> {
        if self.enabled {
            terminal::disable_raw_mode()?;
            self.enabled = false;
        }
        Ok(())
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn items() -> Vec<PickerItem<&'static str>> {
        vec![
            PickerItem::new("low", "low").with_description("broad current-user host authority"),
            PickerItem::new("medium", "medium")
                .with_description("trusted-checkout development")
                .current(true),
            PickerItem::new("high", "high")
                .with_description("inspection-only")
                .with_search_terms(["read only".to_string()]),
        ]
    }

    fn frame(
        items: &[PickerItem<&'static str>],
        query: &str,
        cursor: usize,
        max_items: usize,
    ) -> String {
        let filtered = filtered_indices(items, query);
        let layout = FrameLayout {
            max_items,
            max_width: 80,
        };
        render_picker(
            "Select safety",
            items,
            &filtered,
            query,
            cursor,
            None,
            layout,
        )
    }

    #[test]
    fn digits_only_queries_name_a_menu_number() {
        assert_eq!(query_number(""), None);
        assert_eq!(query_number("   "), None);
        assert_eq!(query_number("0"), Some(0));
        assert_eq!(query_number(" 03 "), Some(3));
        assert_eq!(query_number("12"), Some(12));
        assert_eq!(query_number("3a"), None);
        assert_eq!(query_number("gpt-5"), None);
        assert_eq!(
            query_number("999999999999999999999999999999999999999999"),
            Some(usize::MAX)
        );
    }

    #[test]
    fn enter_prefers_a_typed_number_over_the_highlight() {
        assert_eq!(enter_index(Some(3), 5, 0), Some(2));
        assert_eq!(enter_index(Some(5), 5, 0), Some(4));
        assert_eq!(enter_index(Some(0), 5, 0), None);
        assert_eq!(enter_index(Some(6), 5, 0), None);
        assert_eq!(enter_index(None, 3, 2), Some(2));
        assert_eq!(enter_index(None, 3, 3), None);
        assert_eq!(enter_index(None, 0, 0), None);
    }

    #[test]
    fn number_queries_select_without_filtering() {
        let items = items();
        let layout = FrameLayout {
            max_items: 10,
            max_width: 80,
        };
        // Mirrors `pick`: a numeric query leaves the list unfiltered.
        let filtered = filtered_indices(&items, "");
        let rendered = render_picker("Select safety", &items, &filtered, "2", 1, None, layout);
        assert!(!rendered.contains("matching"));
        assert!(rendered.contains("> 2  medium - trusted-checkout development (current)"));
        assert!(rendered.contains("  3  high - inspection-only"));
        assert!(rendered.contains("Number 2 - Enter to select, Esc to cancel"));
    }

    #[test]
    fn out_of_range_numbers_report_no_selection() {
        let items = items();
        let layout = FrameLayout {
            max_items: 10,
            max_width: 80,
        };
        let filtered = filtered_indices(&items, "");
        let rendered = render_picker("Select safety", &items, &filtered, "9", 0, None, layout);
        assert!(rendered.contains("No selection 9"));
    }

    #[test]
    fn arrow_keys_leave_number_entry_but_keep_text_filters() {
        let mut number = "12".to_string();
        leave_number_entry(&mut number);
        assert!(number.is_empty());

        let mut text = "gpt-5".to_string();
        leave_number_entry(&mut text);
        assert_eq!(text, "gpt-5");
    }

    #[test]
    fn filters_labels_descriptions_and_extra_terms() {
        let items = items();
        assert_eq!(filtered_indices(&items, "MED"), vec![1]);
        assert_eq!(filtered_indices(&items, "inspection"), vec![2]);
        assert_eq!(filtered_indices(&items, "read only"), vec![2]);
        assert_eq!(filtered_indices(&items, "missing"), Vec::<usize>::new());
    }

    #[test]
    fn renders_numbers_cursor_and_current_marker() {
        let items = items();
        let rendered = frame(&items, "", 1, 10);
        assert!(rendered.contains("  1  low - broad current-user host authority"));
        assert!(rendered.contains("> 2  medium - trusted-checkout development (current)"));
        assert!(rendered.contains("  3  high - inspection-only"));
    }

    #[test]
    fn cursor_marker_follows_the_highlight() {
        let items = items();
        let rendered = frame(&items, "", 2, 10);
        assert!(rendered.contains("> 3  high - inspection-only"));
        assert!(rendered.contains("  2  medium - trusted-checkout development (current)"));
    }

    #[test]
    fn filtered_numbers_are_contiguous() {
        let items = items();
        let rendered = frame(&items, "inspection", 0, 10);
        assert!(rendered.contains("Select safety matching \"inspection\""));
        assert!(rendered.contains("> 1  high - inspection-only"));
        assert!(!rendered.contains("  3  high"));
    }

    #[test]
    fn empty_results_render_a_notice_and_survive_enter() {
        let items = items();
        let rendered = frame(&items, "missing", 0, 10);
        assert!(rendered.contains("No matching selections"));
    }

    #[test]
    fn highlight_wraps_at_both_ends() {
        assert_eq!(move_highlight_up(0, 3), 2);
        assert_eq!(move_highlight_down(2, 3), 0);
        assert_eq!(move_highlight_down(1, 3), 2);
        assert_eq!(move_highlight_up(1, 3), 0);
        assert_eq!(move_highlight_down(5, 0), 0);
    }

    #[test]
    fn window_slides_to_keep_the_cursor_visible() {
        assert_eq!(visible_window(0, 3, 10), 0..3);
        assert_eq!(visible_window(0, 100, 10), 0..10);
        assert_eq!(visible_window(50, 100, 10), 45..55);
        assert_eq!(visible_window(99, 100, 10), 90..100);
        assert_eq!(visible_window(0, 100, 0), 0..100);
    }

    #[test]
    fn windowed_frames_renumber_visible_items() {
        let items: Vec<PickerItem<usize>> = (0..30)
            .map(|index| PickerItem::new(index, format!("item {index}")))
            .collect();
        let filtered = filtered_indices(&items, "");
        let layout = FrameLayout {
            max_items: 5,
            max_width: 80,
        };
        let rendered = render_picker("Select", &items, &filtered, "", 29, None, layout);
        assert!(rendered.contains("> 30  item 29"));
        assert!(rendered.contains("  26  item 25"));
        assert!(!rendered.contains("item 24"));
    }

    #[test]
    fn layout_bounds_items_and_columns() {
        let layout = FrameLayout::for_terminal(80, 24);
        assert_eq!(layout.max_items, 19);
        assert_eq!(layout.max_width, 79);
        let narrow = FrameLayout::for_terminal(0, 0);
        assert_eq!(narrow.max_items, 1);
        assert_eq!(narrow.max_width, 1);
    }

    #[test]
    fn resize_events_redraw_with_new_bounds_and_preserve_the_selection() {
        let items: Vec<_> = (0..30)
            .map(|index| {
                PickerItem::new(index, format!("session {index} with a long title"))
                    .current(index == 29)
            })
            .collect();
        for query in ["session", "30"] {
            let mut events = [
                Event::Paste(query.to_string()),
                Event::Resize(32, 8),
                Event::Resize(18, 6),
                Event::Resize(100, 30),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ]
            .into_iter();
            let mut output = Vec::new();
            let selected = run_picker(
                "Select session",
                &items,
                FrameLayout::for_terminal(80, 24),
                &mut output,
                || Ok(events.next().expect("picker requested an unexpected event")),
            )
            .unwrap();
            assert_eq!(selected, Some(29));

            // Each clear starts a new frame. Remove cursor commands but keep the
            // frame's blank lines so its physical height can be checked.
            let output = String::from_utf8(output).unwrap();
            let frames: Vec<_> = output
                .strip_prefix('\r')
                .unwrap()
                .strip_suffix("\r\n")
                .unwrap()
                .split(&Clear(ClearType::FromCursorDown).to_string())
                .map(terminal_text::sanitize)
                .collect();
            let sizes = [(80, 24), (80, 24), (32, 8), (18, 6), (100, 30)];
            assert_eq!(frames.len(), sizes.len(), "each resize must redraw");
            for (index, (frame, (cols, rows))) in frames.iter().zip(sizes).enumerate() {
                let lines: Vec<_> = frame.split("\r\n").collect();
                assert_eq!(lines.len(), rows, "frame {index} has the wrong height");
                assert!(lines.iter().all(|line| line.width() < cols));
                assert!(lines.iter().any(|line| line.starts_with("> 30  ")));
                if index > 0 {
                    let prompt = lines.last().unwrap();
                    assert!(prompt.starts_with(if query == "session" {
                        "Filter: session"
                    } else {
                        "Number 30"
                    }));
                }
            }
        }
    }

    #[test]
    fn long_lines_are_truncated_without_wrapping() {
        let items = vec![PickerItem::new("x", "y".repeat(200)).with_description("z".repeat(200))];
        let filtered = filtered_indices(&items, "");
        let layout = FrameLayout {
            max_items: 10,
            max_width: 40,
        };
        let rendered = render_picker(
            "T".repeat(100).as_str(),
            &items,
            &filtered,
            "",
            0,
            None,
            layout,
        );
        for line in rendered.split("\r\n") {
            assert!(line.width() <= 40, "line too wide: {line:?}");
        }
        assert_eq!(truncate_line("short", 40), "short");
        assert_eq!(truncate_line("abcdef", 5), "ab...");
        assert_eq!(truncate_line("abcdef", 2), "..");
        assert_eq!(truncate_line("abcdef", 1), ".");
        assert_eq!(truncate_line("abcdef", 0), "");
    }

    #[test]
    fn truncation_uses_display_columns_and_keeps_graphemes_intact() {
        for (line, max_width, expected) in [
            ("日本語", 6, "日本語"),
            ("日本語", 5, "日..."),
            ("a界語b", 5, "a..."),
            ("界界", 3, "..."),
            ("界界", 2, ".."),
            ("界", 1, "."),
            ("界", 0, ""),
            ("e\u{301}", 1, "e\u{301}"),
            ("e\u{301}bcdef", 4, "e\u{301}..."),
            ("👩‍💻", 2, "👩‍💻"),
            ("👩‍💻abcd", 5, "👩‍💻..."),
            ("👩‍💻abcd", 4, "..."),
            ("🇷🇴abcd", 5, "🇷🇴..."),
        ] {
            let truncated = truncate_line(line, max_width);
            assert_eq!(truncated, expected, "input {line:?}, width {max_width}");
            assert!(truncated.width() <= max_width);
        }
    }

    #[test]
    fn wide_text_in_every_frame_line_stays_within_terminal_columns() {
        let wide_text = "日本語のセッション".repeat(10);
        let items = vec![
            PickerItem::new(0, &wide_text),
            PickerItem::new(1, "x").with_description(&wide_text),
        ];
        let filtered = filtered_indices(&items, "");
        let layout = FrameLayout::for_terminal(20, 10);
        for notice in [None, Some(wide_text.as_str())] {
            let rendered =
                render_picker(&wide_text, &items, &filtered, &wide_text, 0, notice, layout);
            let lines: Vec<_> = rendered.split("\r\n").collect();
            assert_eq!(lines.len(), 7);
            assert!(lines.iter().any(|line| line.starts_with("> 1  ")));
            for line in lines {
                assert!(line.width() < 20, "line too wide: {line:?}");
            }
        }
    }

    #[test]
    fn capacity_reserves_room_for_frame_chrome() {
        assert_eq!(picker_capacity(24), 19);
        assert_eq!(picker_capacity(3), 1);
        assert_eq!(picker_capacity(0), 1);
    }

    #[test]
    fn initial_and_preferred_cursor_prefer_the_current_item() {
        let items = items();
        assert_eq!(initial_cursor(&items), 1);
        assert_eq!(preferred_cursor(&items, ""), 1);
        assert_eq!(preferred_cursor(&items, "inspection"), 0);
    }

    #[test]
    fn frames_are_single_line_per_entry() {
        let items = vec![PickerItem::new("multi", "line\nbreak").with_description("a\rb")];
        let filtered = filtered_indices(&items, "");
        let layout = FrameLayout {
            max_items: 10,
            max_width: 80,
        };
        let rendered = render_picker("Title\nbreak", &items, &filtered, "", 0, None, layout);
        assert_eq!(rendered.matches("\r\n").count() + 1, 6);
        assert!(rendered.contains("line break - a b"));
    }
}
