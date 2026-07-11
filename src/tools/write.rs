use crate::atomic_file;
use anyhow::{Context, Result};
use std::{fs, path::Path};

pub fn write_text(path: &Path, content: &str) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let expected = atomic_file::target_identity(path)?;
    atomic_file::replace(path, content.as_bytes(), expected)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}
