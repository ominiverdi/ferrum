use crate::terminal_text;
use anyhow::Result;
use crossterm::{
    cursor::MoveToColumn,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType},
};
use std::{
    fmt::Write as FmtWrite,
    io::{self, Write},
};

const MAX_QUERY_CHARS: usize = 256;

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

    let mut query = String::new();
    loop {
        let filtered = filtered_indices(items, &query);
        print!("{}", render_picker(title, items, &filtered, &query));
        io::stdout().flush()?;

        match read_submission()? {
            PickerSubmission::Cancel => return Ok(None),
            PickerSubmission::Number(number) => {
                let Some(filtered_index) = number.checked_sub(1) else {
                    println!("No selection {number}");
                    continue;
                };
                let Some(item_index) = filtered.get(filtered_index) else {
                    println!("No selection {number}");
                    continue;
                };
                return Ok(Some(items[*item_index].value.clone()));
            }
            PickerSubmission::Search(next_query) => query = next_query,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PickerSubmission {
    Cancel,
    Number(usize),
    Search(String),
}

fn parse_submission(input: &str) -> PickerSubmission {
    let input = input.trim();
    if input.is_empty() {
        return PickerSubmission::Cancel;
    }
    if input.chars().all(|character| character.is_ascii_digit()) {
        return PickerSubmission::Number(input.parse().unwrap_or(usize::MAX));
    }
    PickerSubmission::Search(input.to_string())
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

fn render_picker<T>(
    title: &str,
    items: &[PickerItem<T>],
    filtered: &[usize],
    query: &str,
) -> String {
    let mut output = String::new();
    let title = terminal_text::sanitize(title);
    let query = terminal_text::sanitize(query.trim());
    if query.is_empty() {
        let _ = writeln!(output, "\n{title}\n");
    } else {
        let _ = writeln!(output, "\n{title} matching \"{query}\"\n");
    }

    if filtered.is_empty() {
        let _ = writeln!(output, "No matching selections");
    } else {
        let number_width = filtered.len().to_string().len();
        for (display_index, item_index) in filtered.iter().enumerate() {
            let item = &items[*item_index];
            let marker = if item.current { '>' } else { ' ' };
            let label = terminal_text::sanitize(&item.label);
            let description = item
                .description
                .as_deref()
                .map(terminal_text::sanitize)
                .filter(|description| !description.is_empty())
                .map(|description| format!(" - {description}"))
                .unwrap_or_default();
            let _ = writeln!(
                output,
                "{marker} {:>number_width$}  {label}{description}",
                display_index + 1
            );
        }
    }
    output.push('\n');
    output
}

fn read_submission() -> Result<PickerSubmission> {
    const PROMPT: &str = "Enter number, search text, or Esc to return to prompt: ";

    let mut raw_mode = RawModeGuard::enable()?;
    print!("{PROMPT}");
    io::stdout().flush()?;
    let mut input = String::new();

    let outcome = loop {
        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Esc => break PickerSubmission::Cancel,
                    KeyCode::Enter => break parse_submission(&input),
                    KeyCode::Backspace => {
                        input.pop();
                        redraw_input(PROMPT, &input)?;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break PickerSubmission::Cancel;
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL) && input.is_empty() =>
                    {
                        break PickerSubmission::Cancel;
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && input.chars().count() < MAX_QUERY_CHARS =>
                    {
                        input.push(character);
                        redraw_input(PROMPT, &input)?;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                for character in text.chars().filter(|character| !character.is_control()) {
                    if input.chars().count() >= MAX_QUERY_CHARS {
                        break;
                    }
                    input.push(character);
                }
                redraw_input(PROMPT, &input)?;
            }
            _ => {}
        }
    };

    raw_mode.disable()?;
    println!();
    Ok(outcome)
}

fn redraw_input(prompt: &str, input: &str) -> Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    write!(stdout, "{prompt}{}", terminal_text::sanitize(input))?;
    stdout.flush()?;
    Ok(())
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

    #[test]
    fn filters_labels_descriptions_and_extra_terms() {
        let items = items();
        assert_eq!(filtered_indices(&items, "MED"), vec![1]);
        assert_eq!(filtered_indices(&items, "inspection"), vec![2]);
        assert_eq!(filtered_indices(&items, "read only"), vec![2]);
        assert_eq!(filtered_indices(&items, "missing"), Vec::<usize>::new());
    }

    #[test]
    fn parses_numbers_searches_and_cancellation() {
        assert_eq!(parse_submission(" 20 "), PickerSubmission::Number(20));
        assert_eq!(
            parse_submission("999999999999999999999999999999999999999999"),
            PickerSubmission::Number(usize::MAX)
        );
        assert_eq!(
            parse_submission("inspection"),
            PickerSubmission::Search("inspection".to_string())
        );
        assert_eq!(parse_submission("  "), PickerSubmission::Cancel);
    }

    #[test]
    fn renders_numbers_current_marker_and_descriptions() {
        let items = items();
        let rendered = render_picker("Select safety", &items, &[0, 1, 2], "");
        assert!(rendered.contains("  1  low - broad current-user host authority"));
        assert!(rendered.contains("> 2  medium - trusted-checkout development"));
        assert!(rendered.contains("  3  high - inspection-only"));
    }

    #[test]
    fn filtered_numbers_are_contiguous() {
        let items = items();
        let rendered = render_picker("Select safety", &items, &[2], "inspection");
        assert!(rendered.contains("Select safety matching \"inspection\""));
        assert!(rendered.contains("  1  high - inspection-only"));
        assert!(!rendered.contains("  3  high"));
    }
}
