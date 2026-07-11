use crate::{process_containment::CgroupV2, text_truncate::truncate_tail_to_max_bytes};
use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::task;
use uuid::Uuid;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const TERMINATE_GRACE: Duration = Duration::from_millis(200);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const SPOOL_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    Exited,
    TimedOut,
    Cancelled,
}

impl CommandOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandContainment {
    CgroupV2,
    ProcessGroup,
}

impl CommandContainment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CgroupV2 => "cgroup_v2",
            Self::ProcessGroup => "process_group",
        }
    }
}

#[derive(Debug)]
pub struct BashOutput {
    pub status: Option<i32>,
    pub outcome: CommandOutcome,
    pub stdout: String,
    pub stderr: String,
    pub output_incomplete: bool,
    pub output_error: Option<String>,
    pub termination_error: Option<String>,
    pub containment: CommandContainment,
    pub containment_error: Option<String>,
    pub residual_descendants: Option<bool>,
}

#[cfg(test)]
pub async fn run(command: &str, cwd: &Path, timeout: Duration) -> Result<BashOutput> {
    run_with_cancel(command, cwd, timeout, None).await
}

pub async fn run_with_cancel(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<BashOutput> {
    let command = command.to_string();
    let cwd = cwd.to_path_buf();
    task::spawn_blocking(move || run_blocking(&command, &cwd, timeout, cancel))
        .await
        .context("bash worker panicked")?
}

fn run_blocking(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<BashOutput> {
    let (cgroup, containment_error) = match CgroupV2::create("bash") {
        Ok(cgroup) => (Some(cgroup), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let containment = if cgroup.is_some() {
        CommandContainment::CgroupV2
    } else {
        CommandContainment::ProcessGroup
    };

    let mut command_process = Command::new("bash");
    command_process
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(cgroup) = &cgroup {
        cgroup.attach_command(&mut command_process)?;
    }
    let mut child = command_process
        .spawn()
        .context("failed to spawn bash command")?;

    let pid = child.id();
    let stdout = PipeReader::new("stdout", child.stdout.take());
    let stderr = PipeReader::new("stderr", child.stderr.take());
    let (mut stdout, mut stderr) = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (stdout, stderr) => {
            cleanup_failed_spawn(pid, &mut child, cgroup.as_ref());
            let error = stdout
                .err()
                .or_else(|| stderr.err())
                .expect("reader failed");
            return Err(error);
        }
    };
    let deadline = Instant::now() + timeout;

    loop {
        stdout.drain();
        stderr.drain();
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for bash command")?
        {
            return finish_exited_child(
                status,
                stdout,
                stderr,
                cgroup,
                containment,
                containment_error,
            );
        }

        if cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            return finish_interrupted_child(
                pid,
                child,
                stdout,
                stderr,
                cgroup,
                CommandOutcome::Cancelled,
                containment,
                containment_error,
            );
        }

        if Instant::now() >= deadline {
            return finish_interrupted_child(
                pid,
                child,
                stdout,
                stderr,
                cgroup,
                CommandOutcome::TimedOut,
                containment,
                containment_error,
            );
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn finish_exited_child(
    status: ExitStatus,
    mut stdout: PipeReader,
    mut stderr: PipeReader,
    cgroup: Option<CgroupV2>,
    containment: CommandContainment,
    containment_error: Option<String>,
) -> Result<BashOutput> {
    drain_pipes(&mut stdout, &mut stderr, PIPE_DRAIN_TIMEOUT);
    let mut containment_errors = containment_error.into_iter().collect::<Vec<_>>();
    let residual_descendants = if let Some(cgroup) = &cgroup {
        match cgroup.populated() {
            Ok(populated) => Some(populated),
            Err(error) => {
                containment_errors.push(format!(
                    "failed to inspect contained process tree: {error:#}"
                ));
                None
            }
        }
    } else {
        None
    };
    if let Some(cgroup) = &cgroup
        && let Err(error) = cgroup.remove_if_empty()
    {
        containment_errors.push(format!("failed to clean up command cgroup: {error:#}"));
    }
    let containment_error = (!containment_errors.is_empty()).then(|| containment_errors.join("; "));
    build_output(
        status.code(),
        CommandOutcome::Exited,
        stdout,
        stderr,
        None,
        containment,
        containment_error,
        residual_descendants,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_interrupted_child(
    pid: u32,
    mut child: Child,
    mut stdout: PipeReader,
    mut stderr: PipeReader,
    cgroup: Option<CgroupV2>,
    outcome: CommandOutcome,
    containment: CommandContainment,
    containment_error: Option<String>,
) -> Result<BashOutput> {
    let mut termination_errors = Vec::new();
    if let Err(error) = signal_process_group(pid, libc::SIGTERM) {
        termination_errors.push(format!(
            "failed to signal process group with SIGTERM: {error}"
        ));
    }

    let grace_deadline = Instant::now() + TERMINATE_GRACE;
    let mut status = None;
    while Instant::now() < grace_deadline {
        stdout.drain();
        stderr.drain();
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(error) => {
                termination_errors.push(format!("failed to wait after SIGTERM: {error}"));
                break;
            }
        }
    }

    if let Some(cgroup) = &cgroup {
        if let Err(error) = cgroup.kill() {
            termination_errors.push(format!("failed to kill contained process tree: {error:#}"));
            if let Err(error) = signal_process_group(pid, libc::SIGKILL) {
                termination_errors.push(format!(
                    "fallback SIGKILL for process group also failed: {error}"
                ));
            }
        }
    } else if let Err(error) = signal_process_group(pid, libc::SIGKILL) {
        termination_errors.push(format!(
            "failed to signal process group with SIGKILL: {error}"
        ));
    }

    if status.is_none() {
        let reap_deadline = Instant::now() + TERMINATE_GRACE;
        while status.is_none() && Instant::now() < reap_deadline {
            match child.try_wait() {
                Ok(Some(exit)) => status = Some(exit),
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(error) => {
                    termination_errors.push(format!("failed to reap bash command: {error}"));
                    break;
                }
            }
        }
        if status.is_none() {
            if let Err(error) = child.kill() {
                termination_errors.push(format!("failed to kill direct bash child: {error}"));
            }
            match child.try_wait() {
                Ok(Some(exit)) => status = Some(exit),
                Ok(None) => termination_errors
                    .push("bash child did not exit within termination grace".to_string()),
                Err(error) => {
                    termination_errors.push(format!("failed final bash reap check: {error}"))
                }
            }
        }
    }
    drain_pipes(&mut stdout, &mut stderr, PIPE_DRAIN_TIMEOUT);

    let mut containment_errors = containment_error.into_iter().collect::<Vec<_>>();
    let residual_descendants = if let Some(cgroup) = &cgroup {
        let deadline = Instant::now() + TERMINATE_GRACE;
        let mut populated = match cgroup.populated() {
            Ok(populated) => populated,
            Err(error) => {
                containment_errors.push(format!(
                    "failed to inspect contained process tree: {error:#}"
                ));
                true
            }
        };
        while populated && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
            match cgroup.populated() {
                Ok(current) => populated = current,
                Err(error) => {
                    containment_errors.push(format!(
                        "failed to recheck contained process tree: {error:#}"
                    ));
                    break;
                }
            }
        }
        Some(populated)
    } else {
        None
    };
    if let Some(cgroup) = &cgroup
        && let Err(error) = cgroup.remove_if_empty()
    {
        containment_errors.push(format!("failed to clean up command cgroup: {error:#}"));
    }
    let containment_error = (!containment_errors.is_empty()).then(|| containment_errors.join("; "));
    let termination_error = (!termination_errors.is_empty()).then(|| termination_errors.join("; "));
    build_output(
        status.and_then(|status| status.code()),
        outcome,
        stdout,
        stderr,
        termination_error,
        containment,
        containment_error,
        residual_descendants,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_output(
    status: Option<i32>,
    outcome: CommandOutcome,
    stdout: PipeReader,
    stderr: PipeReader,
    termination_error: Option<String>,
    containment: CommandContainment,
    containment_error: Option<String>,
    residual_descendants: Option<bool>,
) -> Result<BashOutput> {
    let stdout = stdout.finish()?;
    let stderr = stderr.finish()?;
    let output_incomplete = stdout.incomplete || stderr.incomplete;
    let output_error = [
        stdout.error.map(|error| format!("stdout: {error}")),
        stderr.error.map(|error| format!("stderr: {error}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    Ok(BashOutput {
        status,
        outcome,
        stdout: stdout.rendered,
        stderr: stderr.rendered,
        output_incomplete,
        output_error: (!output_error.is_empty()).then(|| output_error.join("; ")),
        termination_error,
        containment,
        containment_error,
        residual_descendants,
    })
}

fn cleanup_failed_spawn(pid: u32, child: &mut Child, cgroup: Option<&CgroupV2>) {
    if let Some(cgroup) = cgroup {
        let _ = cgroup.kill();
    }
    let _ = signal_process_group(pid, libc::SIGKILL);
    let _ = child.kill();
    let deadline = Instant::now() + TERMINATE_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(POLL_INTERVAL),
        }
    }
}

fn signal_process_group(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let pgid = -(pid as libc::pid_t);
    let result = unsafe { libc::kill(pgid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

struct PipeReader {
    label: &'static str,
    pipe: Option<Box<dyn Read>>,
    capture: OutputCapture,
    eof: bool,
    error: Option<String>,
}

impl PipeReader {
    fn new(label: &'static str, pipe: Option<impl Read + AsRawFd + 'static>) -> Result<Self> {
        let pipe = pipe
            .map(|pipe| {
                set_nonblocking(pipe.as_raw_fd())?;
                Ok::<Box<dyn Read>, io::Error>(Box::new(pipe))
            })
            .transpose()
            .with_context(|| format!("failed to make bash {label} nonblocking"))?;
        Ok(Self {
            label,
            pipe,
            capture: OutputCapture::default(),
            eof: false,
            error: None,
        })
    }

    fn drain(&mut self) {
        let Some(pipe) = self.pipe.as_mut() else {
            return;
        };
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    self.pipe = None;
                    return;
                }
                Ok(count) => self.capture.append(self.label, &chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.pipe = None;
                    return;
                }
            }
        }
    }

    fn finish(mut self) -> Result<RenderedPipe> {
        let pipe_incomplete = !self.eof;
        self.pipe = None;
        let capture_incomplete = self.capture.incomplete;
        let capture_error = self.capture.error.take();
        let mut rendered = self.capture.render(self.label)?;
        if pipe_incomplete {
            if !rendered.is_empty() && !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push_str("[output incomplete: pipe did not close before drain deadline]\n");
        }
        if capture_incomplete {
            if !rendered.is_empty() && !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push_str("[output incomplete: full stream could not be spooled]\n");
        }
        Ok(RenderedPipe {
            rendered,
            incomplete: pipe_incomplete || capture_incomplete,
            error: self.error.or(capture_error),
        })
    }
}

#[derive(Default)]
struct OutputCapture {
    tail: Vec<u8>,
    spool: Option<(PathBuf, File)>,
    error: Option<String>,
    incomplete: bool,
}

impl OutputCapture {
    fn append(&mut self, label: &str, bytes: &[u8]) {
        if self.spool.is_none() && self.tail.len().saturating_add(bytes.len()) > MAX_OUTPUT_BYTES {
            match create_spool(label).and_then(|(path, mut file)| {
                file.write_all(&self.tail)?;
                Ok((path, file))
            }) {
                Ok(spool) => self.spool = Some(spool),
                Err(error) => self.error = Some(format!("failed to create output spool: {error}")),
            }
        }
        if let Some((_, file)) = &mut self.spool
            && let Err(error) = file.write_all(bytes)
        {
            self.error
                .get_or_insert_with(|| format!("failed to write output spool: {error}"));
            self.spool = None;
        }
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > MAX_OUTPUT_BYTES {
            let excess = self.tail.len() - MAX_OUTPUT_BYTES;
            self.tail.drain(..excess);
            if self.spool.is_none() {
                self.incomplete = true;
            }
        }
    }

    fn render(mut self, label: &str) -> Result<String> {
        if let Some((path, mut file)) = self.spool.take() {
            file.flush()
                .with_context(|| format!("failed to flush {} output spool", path.display()))?;
            let tail = String::from_utf8_lossy(&self.tail);
            let tail = truncate_tail_to_max_bytes(&tail, MAX_OUTPUT_BYTES);
            return Ok(format!(
                "[truncated to last {} bytes. Full output: {}]\n{}",
                MAX_OUTPUT_BYTES,
                path.display(),
                tail
            ));
        }
        let _ = label;
        Ok(String::from_utf8_lossy(&self.tail).into_owned())
    }
}

struct RenderedPipe {
    rendered: String,
    incomplete: bool,
    error: Option<String>,
}

fn set_nonblocking(fd: libc::c_int) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain_pipes(stdout: &mut PipeReader, stderr: &mut PipeReader, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        stdout.drain();
        stderr.drain();
        if (stdout.eof || stdout.error.is_some()) && (stderr.eof || stderr.error.is_some()) {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn create_spool(label: &str) -> io::Result<(PathBuf, File)> {
    let root = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("ferrum"))
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("ferrum-{}", unsafe { libc::geteuid() }))
        });
    ensure_private_directory(&root)?;
    let base = root.join("bash-output");
    ensure_private_directory(&base)?;
    prune_stale_spools(&base);
    let path = base.join(format!("{}-{label}.log", Uuid::new_v4()));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    Ok((path, file))
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("output spool directory is not private: {}", path.display()),
        ));
    }
    Ok(())
}

fn prune_stale_spools(path: &Path) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= SPOOL_RETENTION);
        if !stale {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_spools_full_output_and_bounds_memory() {
        use std::os::unix::fs::PermissionsExt;

        let output = vec![b'x'; MAX_OUTPUT_BYTES + 100];
        let mut capture = OutputCapture::default();
        capture.append("stdout", &output);
        assert_eq!(capture.tail.len(), MAX_OUTPUT_BYTES);
        let rendered = capture.render("stdout").unwrap();
        assert!(rendered.contains("Full output:"));
        let path = rendered
            .lines()
            .next()
            .unwrap()
            .split("Full output: ")
            .nth(1)
            .unwrap()
            .trim_end_matches(']');
        assert_eq!(std::fs::read(path).unwrap(), output);
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn timeout_kills_child_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("child-finished");
        let command = format!(
            "sh -c 'sleep 3; touch {}' & wait",
            shell_quote(marker.to_string_lossy().as_ref())
        );

        let output = run(&command, temp.path(), Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(output.outcome, CommandOutcome::TimedOut);
        assert!(output.termination_error.is_none());

        thread::sleep(Duration::from_millis(3500));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn cancellation_kills_child_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("child-finished");
        let command = format!(
            "sh -c 'sleep 3; touch {}' & wait",
            shell_quote(marker.to_string_lossy().as_ref())
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_task = Arc::clone(&cancel);
        let handle = tokio::spawn(async move {
            run_with_cancel(
                &command,
                temp.path(),
                Duration::from_secs(5),
                Some(cancel_for_task),
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        let cancelled_at = Instant::now();
        cancel.store(true, Ordering::Release);
        let output = handle.await.unwrap();
        assert!(cancelled_at.elapsed() < Duration::from_secs(2));
        assert_eq!(output.outcome, CommandOutcome::Cancelled);
        assert!(output.termination_error.is_none());
    }

    #[tokio::test]
    async fn escaped_descendant_reports_incomplete_output_without_leaking_reader() {
        let temp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let output = run(
            "setsid sh -c 'sleep 3' & echo done",
            temp.path(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(output.stdout.starts_with("done\n"));
        assert!(output.output_incomplete);
        assert!(output.stdout.contains("output incomplete"));
        if output.containment == CommandContainment::CgroupV2 {
            assert_eq!(output.residual_descendants, Some(true));
        } else {
            assert_eq!(output.residual_descendants, None);
        }
    }

    #[tokio::test]
    async fn escaped_writer_is_bounded_and_does_not_leave_a_reader_thread() {
        let temp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let output = run(
            "setsid sh -c 'while :; do printf 1234567890; done' & echo started",
            temp.path(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(output.output_incomplete);
        assert!(output.stdout.len() <= MAX_OUTPUT_BYTES + 256);
    }

    #[tokio::test]
    async fn cancellation_after_process_exit_keeps_exited_outcome() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let handle = tokio::spawn(async move {
            run_with_cancel(
                "setsid sleep 1 & echo completed",
                temp.path(),
                Duration::from_secs(5),
                Some(cancel),
            )
            .await
            .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.store(true, Ordering::Release);
        let output = handle.await.unwrap();

        assert_eq!(output.outcome, CommandOutcome::Exited);
        assert!(output.stdout.starts_with("completed\n"));
        assert!(output.output_incomplete);
    }

    #[tokio::test]
    async fn escaped_descendant_is_contained_on_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("escaped-finished");
        let command = format!(
            "setsid sh -c 'sleep 1; touch {}' & wait",
            shell_quote(marker.to_string_lossy().as_ref())
        );
        let output = run(&command, temp.path(), Duration::from_millis(100))
            .await
            .unwrap();
        if output.containment != CommandContainment::CgroupV2 {
            eprintln!(
                "skipping cgroup assertion: {}",
                output.containment_error.as_deref().unwrap_or("unavailable")
            );
            return;
        }
        assert_eq!(output.outcome, CommandOutcome::TimedOut);
        assert_eq!(output.residual_descendants, Some(false));
        thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn bash_stdin_is_closed() {
        let temp = tempfile::tempdir().unwrap();
        let output = run("cat", temp.path(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(output.outcome, CommandOutcome::Exited);
        assert_eq!(output.stdout, "");
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
