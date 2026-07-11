use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub fn validate_mutation_path(path: &Path, cwd: &Path, roots: &[PathBuf]) -> Result<()> {
    reject_protected_credential_target(path)?;
    let target = resolved_mutation_target(path, cwd)?;
    reject_protected_credential_target(&target)?;
    let roots = canonical_roots(cwd, roots)?;
    if roots.iter().any(|root| target.starts_with(root)) {
        return Ok(());
    }

    let rendered = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "mutation path {} is outside configured writable roots [{}]",
        path.display(),
        rendered
    )
}

pub fn validate_mutation_target(path: &Path, cwd: &Path) -> Result<()> {
    reject_protected_credential_target(path)?;
    let target = resolved_mutation_target(path, cwd)?;
    reject_protected_credential_target(&target)
}

fn reject_protected_credential_target(path: &Path) -> Result<()> {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let protected_credentials = components
        .iter()
        .any(|component| matches!(component.to_str(), Some(".ssh" | ".aws" | ".vault")))
        || components.windows(2).any(|pair| {
            pair[0] == std::ffi::OsStr::new(".config") && pair[1] == std::ffi::OsStr::new("ferrum")
        });
    let protected_system = path.is_absolute()
        && (path.starts_with("/dev")
            || path.starts_with("/proc")
            || path.starts_with("/sys")
            || path.starts_with("/boot")
            || matches!(
                path.to_str(),
                Some("/etc/passwd" | "/etc/shadow" | "/etc/sudoers" | "/etc/hosts")
            ));
    if protected_credentials || protected_system {
        anyhow::bail!(
            "mutation target is protected system or credential state: {}",
            path.display()
        );
    }
    Ok(())
}

fn resolved_mutation_target(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let target = absolute_path(path, cwd)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target)
        && metadata.file_type().is_file()
        && metadata.nlink() > 1
    {
        anyhow::bail!(
            "mutation target has multiple hard links and is not authorized: {}",
            path.display()
        );
    }
    canonicalize_with_missing_tail(&target)
}

pub fn canonical_roots(cwd: &Path, roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if roots.is_empty() {
        anyhow::bail!("no writable roots are configured");
    }
    roots
        .iter()
        .map(|root| {
            let expanded = crate::tools::path::resolve_to_cwd(&root.to_string_lossy(), cwd)?;
            let absolute = absolute_path(&expanded, cwd)?;
            canonicalize_with_missing_tail(&absolute)
                .with_context(|| format!("failed to resolve writable root {}", root.display()))
        })
        .collect()
}

fn absolute_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(cwd)
    };
    Ok(cwd.join(path))
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::<OsString>::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect mutation path {}", path.display())
                });
            }
        }
        let name = existing.file_name().with_context(|| {
            format!("mutation path has no existing ancestor: {}", path.display())
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().with_context(|| {
            format!("mutation path has no existing ancestor: {}", path.display())
        })?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("failed to resolve mutation path {}", path.display()))?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn allows_target_inside_default_root() {
        let temp = tempfile::tempdir().unwrap();
        validate_mutation_path(
            &temp.path().join("new/child.txt"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap();
    }

    #[test]
    fn rejects_lexical_escape() {
        let temp = tempfile::tempdir().unwrap();
        let error = validate_mutation_path(
            &temp.path().join("../outside.txt"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside configured writable roots")
        );
    }

    #[test]
    fn resolves_dotdot_after_symlink_before_authorizing() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let nested = outside.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        symlink(&nested, temp.path().join("link")).unwrap();
        let error = validate_mutation_path(
            &temp.path().join("link/../escaped.txt"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside configured writable roots")
        );
    }

    #[test]
    fn rejects_existing_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("link")).unwrap();
        let error = validate_mutation_path(
            &temp.path().join("link/file.txt"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside configured writable roots")
        );
    }

    #[test]
    fn rejects_hard_link_alias() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target.txt");
        std::fs::write(&target, "keep").unwrap();
        std::fs::hard_link(&target, temp.path().join("linked.txt")).unwrap();
        let error = validate_mutation_path(
            &temp.path().join("linked.txt"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("multiple hard links"));
    }

    #[test]
    fn rejects_dangling_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("future.txt");
        symlink(&target, temp.path().join("link")).unwrap();
        let error = validate_mutation_path(
            &temp.path().join("link"),
            temp.path(),
            &[PathBuf::from(".")],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to resolve mutation path")
        );
        assert!(!target.exists());
    }

    #[test]
    fn target_validation_allows_paths_without_enforcing_roots() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        validate_mutation_target(&outside.path().join("file.txt"), temp.path()).unwrap();
    }

    #[test]
    fn target_validation_still_rejects_hard_link_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target.txt");
        let alias = temp.path().join("linked.txt");
        std::fs::write(&target, "keep").unwrap();
        std::fs::hard_link(&target, &alias).unwrap();

        let error = validate_mutation_target(&alias, temp.path()).unwrap_err();
        assert!(error.to_string().contains("multiple hard links"));
    }

    #[test]
    fn every_tier_validation_rejects_protected_credential_targets() {
        let temp = tempfile::tempdir().unwrap();
        for relative in [
            ".ssh/config",
            ".aws/credentials",
            ".vault/token",
            ".config/ferrum/auth.json",
        ] {
            let target = temp.path().join(relative);
            assert!(validate_mutation_target(&target, temp.path()).is_err());
            assert!(validate_mutation_path(&target, temp.path(), &[PathBuf::from(".")]).is_err());
        }
    }

    #[test]
    fn target_validation_rejects_protected_system_state() {
        for target in [
            "/dev/sda",
            "/proc/sys/kernel/core_pattern",
            "/sys/kernel/test",
            "/boot/loader/entries/test.conf",
            "/etc/passwd",
            "/etc/shadow",
            "/etc/sudoers",
            "/etc/hosts",
        ] {
            assert!(validate_mutation_target(Path::new(target), Path::new("/")).is_err());
        }
    }

    #[test]
    fn protected_target_check_follows_existing_ancestor_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let protected = temp.path().join(".ssh");
        std::fs::create_dir(&protected).unwrap();
        symlink(&protected, temp.path().join("alias")).unwrap();

        let target = temp.path().join("alias/config");
        let error = validate_mutation_target(&target, temp.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("protected system or credential state")
        );
    }

    #[test]
    fn allows_explicit_not_yet_created_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("future/root");
        validate_mutation_path(&root.join("file.txt"), temp.path(), &[root]).unwrap();
    }

    #[test]
    fn allows_explicit_additional_root() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        validate_mutation_path(
            &outside.path().join("file.txt"),
            temp.path(),
            &[PathBuf::from("."), outside.path().to_path_buf()],
        )
        .unwrap();
    }
}
