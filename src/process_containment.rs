use anyhow::{Context, Result};
use std::{
    ffi::CString,
    fs, io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
static LIVE_CGROUPS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct CgroupV2 {
    path: PathBuf,
}

impl CgroupV2 {
    pub(crate) fn create(label: &str) -> Result<Self> {
        let relative = current_cgroup_v2_path()?;
        let parent = Path::new(CGROUP_ROOT).join(relative);
        let mut live = live_cgroups()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_stale_cgroups(&parent, label, &live);
        let path = parent.join(format!(
            "ferrum-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path)
            .with_context(|| format!("failed to create delegated cgroup {}", path.display()))?;
        if !path.join("cgroup.kill").exists() {
            let _ = fs::remove_dir(&path);
            anyhow::bail!("delegated cgroup-v2 hierarchy does not provide cgroup.kill");
        }
        live.insert(path.clone());
        Ok(Self { path })
    }

    pub(crate) fn attach_command(&self, command: &mut Command) -> Result<()> {
        let procs = self.path.join("cgroup.procs");
        let procs = CString::new(procs.as_os_str().as_encoded_bytes())
            .context("cgroup path contains a NUL byte")?;
        unsafe {
            command.pre_exec(move || {
                let fd = libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                let bytes = b"0";
                let written = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                let write_error = if written == bytes.len() as isize {
                    None
                } else {
                    Some(io::Error::last_os_error())
                };
                libc::close(fd);
                if let Some(error) = write_error {
                    return Err(error);
                }
                Ok(())
            });
        }
        Ok(())
    }

    pub(crate) fn kill(&self) -> Result<()> {
        fs::write(self.path.join("cgroup.kill"), b"1")
            .with_context(|| format!("failed to kill cgroup {}", self.path.display()))
    }

    pub(crate) fn wait_empty(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.populated()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn populated(&self) -> Result<bool> {
        cgroup_path_populated(&self.path)
    }

    pub(crate) fn remove_if_empty(&self) -> Result<()> {
        if self.populated()? {
            return Ok(());
        }
        fs::remove_dir(&self.path)
            .with_context(|| format!("failed to remove cgroup {}", self.path.display()))
    }
}

impl Drop for CgroupV2 {
    fn drop(&mut self) {
        live_cgroups()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.path);
        let _ = self.remove_if_empty();
    }
}

fn live_cgroups() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    LIVE_CGROUPS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn prune_stale_cgroups(parent: &Path, label: &str, live: &std::collections::HashSet<PathBuf>) {
    let prefix = format!("ferrum-{label}-");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) || live.contains(&path) {
            continue;
        }
        let owner_pid = name[prefix.len()..]
            .split('-')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok());
        if owner_pid.is_some_and(|pid| {
            pid != std::process::id() && Path::new("/proc").join(pid.to_string()).exists()
        }) {
            continue;
        }
        if cgroup_path_populated(&path).is_ok_and(|populated| !populated) {
            let _ = fs::remove_dir(path);
        }
    }
}

fn cgroup_path_populated(path: &Path) -> Result<bool> {
    let events = fs::read_to_string(path.join("cgroup.events"))
        .with_context(|| format!("failed to read cgroup events for {}", path.display()))?;
    events
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
        .map(|value| value == "1")
        .context("cgroup.events omitted populated state")
}

fn current_cgroup_v2_path() -> Result<PathBuf> {
    let cgroup =
        fs::read_to_string("/proc/self/cgroup").context("failed to read /proc/self/cgroup")?;
    let relative = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("process is not running in a cgroup-v2 hierarchy")?;
    let relative = relative.trim_start_matches('/');
    if relative.split('/').any(|component| component == "..") {
        anyhow::bail!("invalid cgroup-v2 path in /proc/self/cgroup");
    }
    Ok(PathBuf::from(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_cgroup_path_when_v2_is_available() {
        if Path::new(CGROUP_ROOT).join("cgroup.controllers").exists() {
            assert!(current_cgroup_v2_path().is_ok());
        }
    }
}
