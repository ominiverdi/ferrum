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

pub async fn race_with_cancel_grace<F: Future>(
    future: F,
    cancel: Option<&Arc<AtomicBool>>,
    cleanup_grace: Duration,
) -> Result<F::Output, WaitError> {
    tokio::pin!(future);
    tokio::select! {
        biased;
        _ = wait(cancel) => {
            let _ = tokio::time::timeout(cleanup_grace, &mut future).await;
            Err(WaitError::Cancelled)
        },
        output = &mut future => Ok(output),
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
    async fn cancellation_allows_bounded_cooperative_cleanup() {
        struct CleanupMarker(Arc<AtomicBool>);

        impl Drop for CleanupMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        let future_cancel = Arc::clone(&cancel);
        let cleaned_up = Arc::new(AtomicBool::new(false));
        let marker = CleanupMarker(Arc::clone(&cleaned_up));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.store(true, Ordering::Release);
        });

        let future = async move {
            let _marker = marker;
            loop {
                if future_cancel.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    return "cleaned";
                }
                tokio::task::yield_now().await;
            }
        };
        let result =
            race_with_cancel_grace(future, Some(&cancel), Duration::from_millis(100)).await;

        assert_eq!(result, Err(WaitError::Cancelled));
        assert!(cleaned_up.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn timeout_interrupts_pending_future() {
        let result = race_timeout(pending::<()>(), None, Duration::from_millis(10)).await;
        assert_eq!(result, Err(WaitError::TimedOut));
    }
}
