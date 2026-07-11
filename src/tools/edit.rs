use crate::atomic_file;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct EditSpec {
    pub old_text: String,
    pub new_text: String,
}

pub fn replace_exact(path: &Path, edits: &[EditSpec]) -> Result<String> {
    let (original, identity) = atomic_file::read_text_with_identity(path)?;
    let replacements = validate_edits(&original, edits)?;

    let mut output = String::with_capacity(original.len());
    let mut cursor = 0;
    for (start, end, index) in replacements {
        output.push_str(&original[cursor..start]);
        output.push_str(&edits[index].new_text);
        cursor = end;
    }
    output.push_str(&original[cursor..]);

    if output == original {
        anyhow::bail!(
            "no changes made to {}; replacement produced identical content",
            path.display()
        );
    }

    atomic_file::replace(path, output.as_bytes(), Some(identity))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(format!(
        "applied {} edit(s) to {}",
        edits.len(),
        path.display()
    ))
}

fn validate_edits(original: &str, edits: &[EditSpec]) -> Result<Vec<(usize, usize, usize)>> {
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

    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn mixed_lf_crlf_file_preserves_unedited_line_endings() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "alpha\r\nbeta\ngamma\r\n").unwrap();

        replace_exact(
            temp.path(),
            &[EditSpec {
                old_text: "beta\n".into(),
                new_text: "BETA\n".into(),
            }],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(temp.path()).unwrap(),
            "alpha\r\nBETA\ngamma\r\n"
        );
    }

    #[test]
    fn bom_is_preserved() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "\u{feff}alpha\nbeta\n").unwrap();

        replace_exact(
            temp.path(),
            &[EditSpec {
                old_text: "beta".into(),
                new_text: "BETA".into(),
            }],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(temp.path()).unwrap(),
            "\u{feff}alpha\nBETA\n"
        );
    }

    #[test]
    fn overlapping_edits_rejected() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "abcdef\n").unwrap();

        let error = replace_exact(
            temp.path(),
            &[
                EditSpec {
                    old_text: "abc".into(),
                    new_text: "ABC".into(),
                },
                EditSpec {
                    old_text: "bcd".into(),
                    new_text: "BCD".into(),
                },
            ],
        )
        .unwrap_err();

        assert!(error.to_string().contains("overlaps"));
    }
}
