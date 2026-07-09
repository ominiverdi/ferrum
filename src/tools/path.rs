use anyhow::{Context, Result};
use std::{
    env,
    path::{Path, PathBuf},
};

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
    if input == "~" {
        return Ok(home_dir()?.to_string_lossy().into_owned());
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest).to_string_lossy().into_owned());
    }
    if let Some(rest) = input.strip_prefix("file://") {
        return decode_file_url(rest);
    }

    Ok(input.to_string())
}

fn decode_file_url(rest: &str) -> Result<String> {
    let url = url::Url::parse(&format!("file://{rest}"))?;
    url.to_file_path()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|_| anyhow::anyhow!("invalid file URL path"))
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

    #[test]
    fn path_named_at_file_resolves_literally() {
        assert_eq!(
            resolve_to_cwd("@notes.txt", Path::new("/tmp/repo")).unwrap(),
            PathBuf::from("/tmp/repo/@notes.txt")
        );
    }

    #[test]
    fn path_with_nbsp_resolves_literally() {
        assert_eq!(
            resolve_to_cwd("a\u{00a0}b.txt", Path::new("/tmp/repo")).unwrap(),
            PathBuf::from("/tmp/repo/a\u{00a0}b.txt")
        );
    }

    #[test]
    fn file_url_decodes_percent_spaces() {
        assert_eq!(
            resolve_to_cwd("file:///tmp/a%20b.png", Path::new("/repo")).unwrap(),
            PathBuf::from("/tmp/a b.png")
        );
    }
}
