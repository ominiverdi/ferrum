use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
pub struct EditSpec {
    pub old_text: String,
    pub new_text: String,
}

pub fn replace_exact(path: &Path, edits: &[EditSpec]) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let (bom, original_without_bom) = strip_bom(&raw);
    let line_ending = detect_line_ending(original_without_bom);
    let original = normalize_to_lf(original_without_bom);
    let normalized_edits = edits
        .iter()
        .map(|edit| EditSpec {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect::<Vec<_>>();
    validate_edits(&original, &normalized_edits)?;

    let mut replacements = Vec::new();
    for edit in &normalized_edits {
        let start = original
            .find(&edit.old_text)
            .expect("validated edit missing");
        let end = start + edit.old_text.len();
        replacements.push((start, end, edit.new_text.as_str()));
    }
    replacements.sort_by_key(|(start, _, _)| *start);

    let mut output = String::with_capacity(original.len());
    let mut cursor = 0;
    for (start, end, new_text) in replacements {
        output.push_str(&original[cursor..start]);
        output.push_str(new_text);
        cursor = end;
    }
    output.push_str(&original[cursor..]);

    if output == original {
        anyhow::bail!(
            "no changes made to {}; replacement produced identical content",
            path.display()
        );
    }

    let final_output = format!("{bom}{}", restore_line_endings(&output, line_ending));
    fs::write(path, final_output).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(format!(
        "applied {} edit(s) to {}",
        edits.len(),
        path.display()
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

fn strip_bom(content: &str) -> (&str, &str) {
    content
        .strip_prefix('\u{feff}')
        .map_or(("", content), |text| ("\u{feff}", text))
}

fn detect_line_ending(content: &str) -> LineEnding {
    let crlf = content.find("\r\n");
    let lf = content.find('\n');
    match (crlf, lf) {
        (Some(crlf), Some(lf)) if crlf <= lf => LineEnding::Crlf,
        _ => LineEnding::Lf,
    }
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, line_ending: LineEnding) -> String {
    match line_ending {
        LineEnding::Lf => text.to_string(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

fn validate_edits(original: &str, edits: &[EditSpec]) -> Result<()> {
    if edits.is_empty() {
        anyhow::bail!("edits must not be empty");
    }

    let mut ranges = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            anyhow::bail!("edit {index} old_text must not be empty");
        }
        let matches: Vec<_> = original.match_indices(&edit.old_text).collect();
        match matches.len() {
            0 => anyhow::bail!("edit {index} old_text was not found"),
            1 => {
                let start = matches[0].0;
                ranges.push((start, start + edit.old_text.len(), index));
            }
            count => {
                anyhow::bail!("edit {index} old_text matched {count} times; expected exactly once")
            }
        }
    }

    ranges.sort_by_key(|(start, _, _)| *start);
    for pair in ranges.windows(2) {
        let (_, previous_end, previous_index) = pair[0];
        let (next_start, _, next_index) = pair[1];
        if previous_end > next_start {
            anyhow::bail!("edit {previous_index} overlaps edit {next_index}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_multiple_non_overlapping_edits() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "alpha\nbeta\ngamma\n").unwrap();
        replace_exact(
            temp.path(),
            &[
                EditSpec {
                    old_text: "alpha".into(),
                    new_text: "one".into(),
                },
                EditSpec {
                    old_text: "gamma".into(),
                    new_text: "three".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(temp.path()).unwrap(),
            "one\nbeta\nthree\n"
        );
    }

    #[test]
    fn rejects_duplicate_old_text() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "same\nsame\n").unwrap();
        let error = replace_exact(
            temp.path(),
            &[EditSpec {
                old_text: "same".into(),
                new_text: "other".into(),
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("matched 2 times"));
    }
}
