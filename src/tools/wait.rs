use crate::tools::bash;
use anyhow::Result;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time;

const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

pub async fn run(
    command: &str,
    cwd: &Path,
    wait: Duration,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
    progress: bool,
) -> Result<bash::BashOutput> {
    let mut elapsed = Duration::ZERO;
    while elapsed < wait {
        if cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            anyhow::bail!("aborted");
        }
        if progress {
            render_progress(elapsed, wait);
        }
        let step = PROGRESS_INTERVAL.min(wait - elapsed);
        time::sleep(step).await;
        elapsed += step;
    }
    if progress {
        render_progress(wait, wait);
        eprint!("\r\n");
    }
    bash::run_with_cancel(command, cwd, timeout, cancel).await
}

fn render_progress(elapsed: Duration, wait: Duration) {
    let total = wait.as_secs();
    let done = elapsed.as_secs().min(total);
    let remaining = total.saturating_sub(done);
    eprint!("\r[wait] {done}/{total}s elapsed, {remaining}s remaining; Esc or Ctrl-C aborts");
}
