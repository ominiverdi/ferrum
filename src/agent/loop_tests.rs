use super::context_pressure_tests::{RecordingSink, test_config};
use super::*;

fn loop_test_config(path: &Path) -> Config {
    let mut config = test_config(path.to_path_buf());
    config.max_context_tokens = 50_000;
    config.base_max_context_tokens = 50_000;
    config
}

#[tokio::test]
async fn tool_sequence_loop_is_bounded_and_persists_matched_results() {
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    std::fs::write(temp.path().join("loop.txt"), "first line\nsecond line\n").unwrap();
    let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
    let mut sink = RecordingSink::default();
    let outcome = state
        .run_turn_with_events(
            "__ferrum_test_loop_guard_loop_sequence__".to_string(),
            &config,
            TurnOptions::headless(events::TurnCancellation::new()),
            &mut sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        sink.events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallCompleted { .. }))
            .count(),
        6
    );
    assert!(sink.events.iter().any(|event| matches!(event, AgentEvent::Notice { message, .. } if message.contains("2-call tool sequence repeated 2 times"))));
    assert!(sink.events.iter().any(|event| matches!(event, AgentEvent::Notice { message, .. } if message.contains("2-call tool sequence repeated 3 times"))));
    assert_eq!(
        state.messages.last().unwrap().text_content(),
        "final after tool sequence loop guard\n"
    );
    let loaded = session::jsonl::load_messages(state.session.path()).unwrap();
    let calls = loaded
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| matches!(block, messages::ContentBlock::ToolUse { .. }))
        .count();
    let results = loaded
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| matches!(block, messages::ContentBlock::ToolResult { .. }))
        .count();
    assert_eq!(calls, results);
    assert_eq!(calls, 6);
}

#[tokio::test]
async fn stream_loop_recovers_without_persisting_partial_text_or_reasoning() {
    for script in ["repeat_text_recover", "repeat_thinking"] {
        let temp = tempfile::tempdir().unwrap();
        let config = loop_test_config(temp.path());
        let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
        let mut sink = RecordingSink::default();
        let cancellation = events::TurnCancellation::new();
        let outcome = state
            .run_turn_with_events(
                format!("__ferrum_test_loop_guard_{script}__"),
                &config,
                TurnOptions::headless(cancellation.clone()),
                &mut sink,
            )
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ModelRequestStarted { .. }))
                .count(),
            2
        );
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, AgentEvent::AssistantMessage { .. }))
                .count(),
            1
        );
        let streamed = sink
            .events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta(text) | AgentEvent::ThinkingDelta(text) => Some(text.len()),
                _ => None,
            })
            .sum::<usize>();
        assert!(
            streamed < 1_000,
            "loop was not interrupted early: {streamed}"
        );
        assert_eq!(
            state.messages.last().unwrap().text_content(),
            "recovered concise response\n"
        );
        let persisted = std::fs::read_to_string(state.session.path()).unwrap();
        assert!(!persisted.contains("This response repeats"));
        assert!(!persisted.contains("fake-interrupted-signature"));
        assert!(!persisted.contains("Its partial output was discarded"));
    }
}

#[tokio::test]
async fn stream_loop_escalates_to_one_final_synthesis() {
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
    let mut sink = RecordingSink::default();
    assert_eq!(
        state
            .run_turn_with_events(
                "__ferrum_test_loop_guard_repeat_then_final__".to_string(),
                &config,
                TurnOptions::headless(events::TurnCancellation::new()),
                &mut sink,
            )
            .await
            .unwrap(),
        TurnOutcome::Completed
    );
    assert_eq!(
        sink.events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ModelRequestStarted { .. }))
            .count(),
        3
    );
    assert!(sink.events.iter().any(|event| matches!(
        event,
        AgentEvent::ModelRequestStarted {
            kind: ModelRequestKind::FinalSynthesis,
            ..
        }
    )));
    assert_eq!(
        state.messages.last().unwrap().text_content(),
        "recovered concise response\n"
    );
}

#[tokio::test]
async fn persistent_stream_loop_stops_without_endless_recovery() {
    for streaming in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let config = loop_test_config(temp.path());
        let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
        let mut sink = RecordingSink::default();
        let cancellation = events::TurnCancellation::new();
        let mut options = TurnOptions::headless(cancellation.clone());
        options.stream_responses = streaming;
        let error = state
            .run_turn_with_events(
                "__ferrum_test_loop_guard_repeat_text__".to_string(),
                &config,
                options,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("stopped without further retries"),
            "{error:#}"
        );
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ModelRequestStarted { .. }))
                .count(),
            3
        );
        assert!(
            !state
                .messages
                .iter()
                .any(|m| m.role == messages::Role::Assistant)
        );
        assert!(
            !sink.events.iter().any(|event| matches!(
                event,
                AgentEvent::TurnCompleted | AgentEvent::TurnCancelled
            ))
        );
    }
}

#[tokio::test]
async fn interrupted_response_never_persists_or_executes_its_tool_calls() {
    for streaming in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let config = loop_test_config(temp.path());
        let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
        let mut options = TurnOptions::headless(events::TurnCancellation::new());
        options.stream_responses = streaming;
        let mut sink = RecordingSink::default();
        let error = state
            .run_turn_with_events(
                "__ferrum_test_loop_guard_repeat_with_tool__".to_string(),
                &config,
                options,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("stopped without further retries")
        );
        assert!(!temp.path().join("interrupted-write.txt").exists());
        assert!(!sink.events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallStarted { .. }
                | AgentEvent::ToolCallCompleted { .. }
                | AgentEvent::AssistantMessage { .. }
        )));
        let persisted = std::fs::read_to_string(state.session.path()).unwrap();
        assert!(!persisted.contains("fake-interrupted-write"));
        assert!(!persisted.contains("This response repeats"));
        let usage = std::fs::read_to_string(config.data_dir.join("usage.jsonl")).unwrap();
        assert_eq!(usage.lines().count(), 3);
    }
}

#[tokio::test]
async fn failed_provider_response_retains_error_even_if_final_tail_repeats() {
    struct FailedProvider;
    impl providers::Provider for FailedProvider {
        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [messages::Message],
            _tools: &'a [tools::ToolDefinition],
            _thinking: ThinkingLevel,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<providers::ProviderResponse>> + Send + 'a>>
        {
            Box::pin(async { anyhow::bail!("test provider transport failure") })
        }
        fn complete_streaming<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [messages::Message],
            _tools: &'a [tools::ToolDefinition],
            _thinking: ThinkingLevel,
            on_event: &'a mut (dyn FnMut(providers::StreamEvent) + Send),
            _cancelled: Option<Arc<AtomicBool>>,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<providers::ProviderResponse>> + Send + 'a>>
        {
            Box::pin(async move {
                let block = (0..100)
                    .map(|i| char::from_u32(0x4e00 + i).unwrap())
                    .collect::<String>();
                on_event(providers::StreamEvent::TextDelta(block.repeat(3)));
                anyhow::bail!("test provider transport failure")
            })
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    let options = TurnOptions::headless(events::TurnCancellation::new());
    let mut timer = ModelPerformanceTimer::start(true);
    let mut sink = RecordingSink::default();
    let error = complete_model_request(
        &FailedProvider,
        &config,
        &[],
        &[],
        &options,
        &mut sink,
        &mut timer,
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "test provider transport failure");
    assert!(error.downcast_ref::<InterruptedResponse>().is_none());
}

#[tokio::test]
async fn user_cancellation_during_stream_loop_never_starts_recovery() {
    struct CancellingSink {
        cancellation: events::TurnCancellation,
        requests: usize,
    }
    impl AgentEventSink for CancellingSink {
        fn emit(&mut self, event: AgentEvent) -> Result<()> {
            if matches!(event, AgentEvent::ModelRequestStarted { .. }) {
                self.requests += 1;
            }
            if let AgentEvent::TextDelta(text) = event
                && text.contains("progress")
            {
                self.cancellation.cancel();
            }
            Ok(())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
    let cancellation = events::TurnCancellation::new();
    let mut sink = CancellingSink {
        cancellation: cancellation.clone(),
        requests: 0,
    };
    let result = state
        .run_turn_with_events(
            "__ferrum_test_loop_guard_repeat_text__".to_string(),
            &config,
            TurnOptions::headless(cancellation.clone()),
            &mut sink,
        )
        .await
        .unwrap();
    assert_eq!(result, TurnOutcome::Cancelled);
    assert!(cancellation.is_cancelled());
    assert_eq!(sink.requests, 1);
    assert!(
        !state
            .messages
            .iter()
            .any(|m| m.role == messages::Role::Assistant)
    );
}

#[tokio::test]
async fn stream_loop_cleanup_is_bounded_for_uncooperative_provider() {
    struct IgnoringProvider(Arc<AtomicBool>);
    impl providers::Provider for IgnoringProvider {
        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [messages::Message],
            _tools: &'a [tools::ToolDefinition],
            _thinking: ThinkingLevel,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<providers::ProviderResponse>> + Send + 'a>>
        {
            Box::pin(std::future::pending())
        }
        fn complete_streaming<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [messages::Message],
            _tools: &'a [tools::ToolDefinition],
            _thinking: ThinkingLevel,
            on_event: &'a mut (dyn FnMut(providers::StreamEvent) + Send),
            _cancelled: Option<Arc<AtomicBool>>,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<providers::ProviderResponse>> + Send + 'a>>
        {
            struct Dropped(Arc<AtomicBool>);
            impl Drop for Dropped {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            Box::pin(async move {
                let _dropped = Dropped(Arc::clone(&self.0));
                on_event(providers::StreamEvent::TextDelta(
                    "An uncooperative provider keeps producing the same explanation instead of making a different concrete observation. ".repeat(10),
                ));
                std::future::pending().await
            })
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    let dropped = Arc::new(AtomicBool::new(false));
    let provider = IgnoringProvider(Arc::clone(&dropped));
    let cancellation = events::TurnCancellation::new();
    let options = TurnOptions::headless(cancellation.clone());
    let mut timer = ModelPerformanceTimer::start(true);
    let mut sink = RecordingSink::default();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        complete_model_request(
            &provider,
            &config,
            &[],
            &[],
            &options,
            &mut sink,
            &mut timer,
        ),
    )
    .await
    .expect("uncooperative provider cleanup hung")
    .unwrap_err();
    assert!(error.downcast_ref::<InterruptedResponse>().is_some());
    assert!(!cancellation.is_cancelled());
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn stream_event_sink_failure_is_not_treated_as_loop_recovery() {
    struct FailingSink;
    impl AgentEventSink for FailingSink {
        fn emit(&mut self, event: AgentEvent) -> Result<()> {
            if matches!(event, AgentEvent::TextDelta(_)) {
                anyhow::bail!("test sink disconnected");
            }
            Ok(())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = loop_test_config(temp.path());
    let mut state = AgentSession::new_at_cwd(&config, temp.path().to_path_buf()).unwrap();
    let error = state
        .run_turn_with_events(
            "__ferrum_test_loop_guard_repeat_text__".to_string(),
            &config,
            TurnOptions::headless(events::TurnCancellation::new()),
            &mut FailingSink,
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "test sink disconnected");
    assert!(
        !state
            .messages
            .iter()
            .any(|m| m.role == messages::Role::Assistant)
    );
}
