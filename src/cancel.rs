use std::{
    future::{Future, pending},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    Cancelled,
    TimedOut,
}

pub async fn wait(cancel: Option<&Arc<AtomicBool>>) {
    let Some(cancel) = cancel else {
        pending::<()>().await;
        return;
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

pub async fn race<F: Future>(
    future: F,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<F::Output, WaitError> {
    tokio::select! {
        biased;
        _ = wait(cancel) => Err(WaitError::Cancelled),
        output = future => Ok(output),
    }
}

pub async fn race_timeout<F: Future>(
    future: F,
    cancel: Option<&Arc<AtomicBool>>,
    timeout: Duration,
) -> Result<F::Output, WaitError> {
    tokio::select! {
        biased;
        _ = wait(cancel) => Err(WaitError::Cancelled),
        _ = tokio::time::sleep(timeout) => Err(WaitError::TimedOut),
        output = future => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_interrupts_pending_future() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.store(true, Ordering::Relaxed);
        });
        let result =
            tokio::time::timeout(Duration::from_secs(1), race(pending::<()>(), Some(&cancel)))
                .await
                .expect("cancellation was not observed");
        assert_eq!(result, Err(WaitError::Cancelled));
    }

    #[tokio::test]
    async fn timeout_interrupts_pending_future() {
        let result = race_timeout(pending::<()>(), None, Duration::from_millis(10)).await;
        assert_eq!(result, Err(WaitError::TimedOut));
    }
}
