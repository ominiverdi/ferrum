use super::messages::{Message, TokenUsage};
use anyhow::Result;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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

#[derive(Debug, Default)]
pub struct IgnoreAgentEvents;

impl AgentEventSink for IgnoreAgentEvents {
    fn emit(&mut self, _event: AgentEvent) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnCancellation {
    cancelled: Arc<AtomicBool>,
}

impl TurnCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct TurnOptions {
    pub stream_responses: bool,
    pub monitor_terminal_cancel: bool,
    pub cancellation: TurnCancellation,
}

impl TurnOptions {
    pub fn headless(cancellation: TurnCancellation) -> Self {
        Self {
            stream_responses: true,
            monitor_terminal_cancel: false,
            cancellation,
        }
    }

    pub(crate) fn terminal(interactive: bool) -> Self {
        Self {
            stream_responses: interactive,
            monitor_terminal_cancel: interactive,
            cancellation: TurnCancellation::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Cancelled,
}
