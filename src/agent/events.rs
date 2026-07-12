use super::messages::{Message, TokenUsage};
use anyhow::Result;
use serde_json::Value;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TurnStarted {
        cwd: PathBuf,
    },
    ModelRequestStarted {
        request: usize,
        kind: ModelRequestKind,
    },
    ThinkingDelta(String),
    TextDelta(String),
    AssistantMessage {
        message: Message,
    },
    UsageUpdated {
        usage: TokenUsage,
        estimated_context_tokens: usize,
    },
    ToolCallStarted {
        id: String,
        name: String,
        input: Value,
    },
    ToolCallCompleted {
        id: String,
        name: String,
        input: Value,
        content: String,
        is_error: bool,
        aborted: bool,
        duration_ms: u128,
    },
    Notice {
        kind: NoticeKind,
        message: String,
    },
    TurnCancelled,
    TurnCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRequestKind {
    Agent,
    FinalSynthesis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Diagnostic,
    Status,
}

pub trait AgentEventSink: Send {
    fn emit(&mut self, event: AgentEvent) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct ToolPermissionRequest {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub cancellation: TurnCancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermissionDecision {
    Allow,
    Reject,
    Cancelled,
}

pub trait ToolPermissionHandler: Send + Sync {
    fn request(
        &self,
        request: ToolPermissionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ToolPermissionDecision>> + Send + '_>>;
}

#[derive(Debug, Default)]
pub struct IgnoreAgentEvents;

impl AgentEventSink for IgnoreAgentEvents {
    fn emit(&mut self, _event: AgentEvent) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct TurnCancellationInner {
    cancelled: Arc<AtomicBool>,
    notify: Notify,
}

impl Default for TurnCancellationInner {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Notify::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnCancellation {
    inner: Arc<TurnCancellationInner>,
}

impl TurnCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.inner.cancelled)
    }
}

#[derive(Clone)]
pub struct TurnOptions {
    pub stream_responses: bool,
    pub monitor_terminal_cancel: bool,
    pub cancellation: TurnCancellation,
    pub permission_handler: Option<Arc<dyn ToolPermissionHandler>>,
}

impl TurnOptions {
    pub fn headless(cancellation: TurnCancellation) -> Self {
        Self {
            stream_responses: true,
            monitor_terminal_cancel: false,
            cancellation,
            permission_handler: None,
        }
    }

    pub fn with_permission_handler(mut self, handler: Arc<dyn ToolPermissionHandler>) -> Self {
        self.permission_handler = Some(handler);
        self
    }

    pub(crate) fn terminal(interactive: bool) -> Self {
        Self {
            stream_responses: interactive,
            monitor_terminal_cancel: interactive,
            cancellation: TurnCancellation::new(),
            permission_handler: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Cancelled,
}
