use anyhow::{Context, Result};
use rand::{RngCore, rngs::OsRng};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetIdentity {
    dev: u64,
    ino: u64,
    len: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    mode: u32,
    uid: u32,
    gid: u32,
}

impl TargetIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            mode: metadata.mode() & 0o777,
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }

    fn mode(self) -> u32 {
        self.mode
    }

    fn owner(self) -> (u32, u32) {
        (self.uid, self.gid)
    }
}

pub fn target_identity(path: &Path) -> Result<Option<TargetIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                anyhow::bail!("refusing to replace symlink target: {}", path.display());
            }
            if !metadata.is_file() {
                anyhow::bail!("mutation target is not a regular file: {}", path.display());
            }
            Ok(Some(TargetIdentity::from_metadata(&metadata)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

pub fn read_text_with_identity(path: &Path) -> Result<(String, TargetIdentity)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !before.is_file() {
        anyhow::bail!("edit target is not a regular file: {}", path.display());
    }
    let before = TargetIdentity::from_metadata(&before);
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let after = TargetIdentity::from_metadata(
        &file
            .metadata()
            .with_context(|| format!("failed to recheck {}", path.display()))?,
    );
    if before != after {
        anyhow::bail!("target changed while it was being read: {}", path.display());
    }
    Ok((text, before))
}

pub fn replace(path: &Path, bytes: &[u8], expected: Option<TargetIdentity>) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    verify_target(path, expected)?;
    let (temp_path, mut temp_file) = create_sibling_temp(path, parent)?;
    let creation_mode = temp_file
        .metadata()
        .with_context(|| format!("failed to inspect temporary file for {}", path.display()))?
        .mode()
        & 0o777;
    temp_file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect temporary file for {}", path.display()))?;
    let mode = expected.map(TargetIdentity::mode).unwrap_or(creation_mode);
    let mut cleanup = TempCleanup(Some(temp_path.clone()));

    temp_file
        .write_all(bytes)
        .with_context(|| format!("failed to write temporary file for {}", path.display()))?;
    temp_file
        .flush()
        .with_context(|| format!("failed to flush temporary file for {}", path.display()))?;
    if let Some(expected) = expected {
        let (uid, gid) = expected.owner();
        let result = unsafe { libc::fchown(temp_file.as_raw_fd(), uid, gid) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to preserve ownership for {}", path.display()));
        }
    }
    temp_file
        .set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set temporary permissions for {}", path.display()))?;
    temp_file
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for {}", path.display()))?;
    drop(temp_file);

    verify_target(path, expected)?;
    replace_path_atomically(&temp_path, path, expected)?;
    cleanup.0 = None;

    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync directory {}", parent.display()))?;
    Ok(())
}

fn replace_path_atomically(
    temp_path: &Path,
    target_path: &Path,
    expected: Option<TargetIdentity>,
) -> Result<()> {
    match expected {
        None => renameat2(temp_path, target_path, libc::RENAME_NOREPLACE)
            .with_context(|| format!("failed to create {} atomically", target_path.display())),
        Some(expected) => {
            renameat2(temp_path, target_path, libc::RENAME_EXCHANGE).with_context(|| {
                format!("failed to exchange {} atomically", target_path.display())
            })?;
            let displaced = match fs::symlink_metadata(temp_path) {
                Ok(metadata) => TargetIdentity::from_metadata(&metadata),
                Err(error) => {
                    renameat2(temp_path, target_path, libc::RENAME_EXCHANGE).with_context(
                        || {
                            format!(
                                "displaced mutation target disappeared and rollback failed: {}",
                                target_path.display()
                            )
                        },
                    )?;
                    return Err(error).with_context(|| {
                        format!(
                            "displaced mutation target disappeared; replacement rolled back: {}",
                            target_path.display()
                        )
                    });
                }
            };
            if !same_file_version(displaced, expected) {
                renameat2(temp_path, target_path, libc::RENAME_EXCHANGE).with_context(|| {
                    format!(
                        "mutation target changed and rollback failed: {}",
                        target_path.display()
                    )
                })?;
                anyhow::bail!(
                    "mutation target changed during atomic replacement: {}",
                    target_path.display()
                );
            }
            if let Err(error) = fs::remove_file(temp_path) {
                renameat2(temp_path, target_path, libc::RENAME_EXCHANGE).with_context(|| {
                    format!(
                        "failed to remove old copy and rollback replacement of {}: {error}",
                        target_path.display()
                    )
                })?;
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove old copy; replacement rolled back: {}",
                        target_path.display()
                    )
                });
            }
            Ok(())
        }
    }
}

fn same_file_version(left: TargetIdentity, right: TargetIdentity) -> bool {
    left.dev == right.dev
        && left.ino == right.ino
        && left.len == right.len
        && left.modified_seconds == right.modified_seconds
        && left.modified_nanoseconds == right.modified_nanoseconds
}

fn renameat2(old_path: &Path, new_path: &Path, flags: libc::c_uint) -> Result<()> {
    let old = CString::new(old_path.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL: {}", old_path.display()))?;
    let new = CString::new(new_path.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL: {}", new_path.display()))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

fn verify_target(path: &Path, expected: Option<TargetIdentity>) -> Result<()> {
    let current = target_identity(path)?;
    if current != expected {
        anyhow::bail!(
            "mutation target changed before replacement: {}",
            path.display()
        );
    }
    Ok(())
}

fn create_sibling_temp(path: &Path, parent: &Path) -> Result<(PathBuf, File)> {
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    for _ in 0..128 {
        let mut random = [0u8; 12];
        OsRng.fill_bytes(&mut random);
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let temp_path = parent.join(format!(".{stem}.ferrum-{suffix}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create temporary file in {}", parent.display())
                });
            }
        }
    }
    anyhow::bail!(
        "failed to create unique temporary file in {}",
        parent.display()
    )
}

struct TempCleanup(Option<PathBuf>);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_content_and_preserves_mode() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let identity = target_identity(&path).unwrap();

        replace(&path, b"new", identity).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), unsafe {
            (libc::geteuid(), libc::getegid())
        });
    }

    #[test]
    fn detects_target_swap_before_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        fs::write(&path, "old").unwrap();
        let identity = target_identity(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, "replacement").unwrap();

        let error = replace(&path, b"new", identity).unwrap_err();

        assert!(error.to_string().contains("changed before replacement"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
    }

    #[test]
    fn new_file_replace_does_not_overwrite_concurrent_creation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        fs::write(&path, "concurrent").unwrap();

        let error = replace(&path, b"new", None).unwrap_err();

        assert!(error.to_string().contains("changed before replacement"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "concurrent");
    }

    #[test]
    fn no_replace_commit_does_not_overwrite_racing_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("file.txt");
        let staged = temp.path().join("staged.txt");
        fs::write(&target, "concurrent").unwrap();
        fs::write(&staged, "new").unwrap();

        let error = replace_path_atomically(&staged, &target, None).unwrap_err();

        assert!(error.to_string().contains("failed to create"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "concurrent");
        assert_eq!(fs::read_to_string(&staged).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_destination() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real.txt");
        let link = temp.path().join("link.txt");
        fs::write(&real, "old").unwrap();
        symlink(&real, &link).unwrap();

        let error = target_identity(&link).unwrap_err();
        assert!(error.to_string().contains("symlink"));
    }
}
