use anyhow::{Context, Result};
use std::{
    env,
    path::{Path, PathBuf},
};

const UNICODE_SPACES: &[char] = &[
    '\u{00A0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
    '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
];

pub fn resolve_to_cwd(input: &str, cwd: &Path) -> Result<PathBuf> {
    let normalized = normalize_path(input)?;
    let path = PathBuf::from(normalized);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn normalize_path(input: &str) -> Result<String> {
    let mut value = input
        .chars()
        .map(|ch| {
            if UNICODE_SPACES.contains(&ch) {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>();

    if let Some(stripped) = value.strip_prefix('@') {
        value = stripped.to_string();
    }

    if value == "~" {
        return Ok(home_dir()?.to_string_lossy().into_owned());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest).to_string_lossy().into_owned());
    }
    if let Some(rest) = value.strip_prefix("file://") {
        return Ok(rest.to_string());
    }

    Ok(value)
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_to_cwd() {
        assert_eq!(
            resolve_to_cwd("src/main.rs", Path::new("/tmp/repo")).unwrap(),
            PathBuf::from("/tmp/repo/src/main.rs")
        );
    }

    #[test]
    fn absolute_path_stays_absolute() {
        assert_eq!(
            resolve_to_cwd("/tmp/file", Path::new("/repo")).unwrap(),
            PathBuf::from("/tmp/file")
        );
    }
}
