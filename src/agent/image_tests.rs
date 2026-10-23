use super::context_pressure_tests::{RecordingSink, test_config};
use super::*;

fn write_image(path: &Path, format: image::ImageFormat) {
    let mut data = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut data, format)
        .unwrap();
    fs::write(path, data.into_inner()).unwrap();
}

#[tokio::test]
async fn read_image_reaches_next_model_request_and_survives_resume() {
    for parallel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config(root.path().to_path_buf());
        config.max_context_tokens = 100_000;
        config.base_max_context_tokens = 100_000;
        write_image(&root.path().join("image.png"), image::ImageFormat::Png);
        write_image(&root.path().join("second.webp"), image::ImageFormat::WebP);
        let mut state = AgentSession::new_at_cwd(&config, root.path().to_path_buf()).unwrap();
        let mut sink = RecordingSink::default();
        let prompt = if parallel {
            "__ferrum_test_read_images__"
        } else {
            "__ferrum_test_read_image__"
        };
        let outcome = state
            .run_turn_with_events(
                prompt.to_string(),
                &config,
                TurnOptions::headless(events::TurnCancellation::new()),
                &mut sink,
            )
            .await
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        let expected = if parallel { 2 } else { 1 };
        assert!(
            state
                .messages
                .last()
                .unwrap()
                .text_content()
                .contains(&format!("provider image blocks={expected}:"))
        );
        let results = state
            .messages
            .iter()
            .filter(|message| matches!(message.role, messages::Role::Tool))
            .collect::<Vec<_>>();
        assert_eq!(results.len(), expected);
        for result in results {
            assert!(matches!(
                result.content[0],
                messages::ContentBlock::ToolResult {
                    is_error: false,
                    ..
                }
            ));
            assert!(matches!(
                result.content[1],
                messages::ContentBlock::Image { .. }
            ));
        }
        for event in &sink.events {
            if let AgentEvent::ToolCallCompleted {
                content, is_error, ..
            } = event
            {
                assert!(!is_error);
                assert!(content.contains("image content attached"));
                assert!(!content.contains("base64"));
                assert!(content.len() < 512);
            }
        }
        let path = state.session.path().clone();
        let restored =
            AgentSession::open_session_at_cwd(&config, path, root.path().to_path_buf()).unwrap();
        assert_eq!(
            image_totals(
                restored
                    .messages
                    .iter()
                    .flat_map(|message| message.content.iter())
            )
            .0,
            expected
        );
    }
}

#[tokio::test]
async fn read_image_for_text_only_model_reports_tool_error_without_attaching() {
    let root = tempfile::tempdir().unwrap();
    let mut config = test_config(root.path().to_path_buf());
    config.max_context_tokens = 100_000;
    config.base_max_context_tokens = 100_000;
    config.models.insert(
        config.model.clone(),
        crate::config::ModelDefinition {
            provider: None,
            actual_model: None,
            max_context_tokens: None,
            supports_images: Some(false),
        },
    );
    write_image(&root.path().join("image.png"), image::ImageFormat::Png);
    let mut state = AgentSession::new_at_cwd(&config, root.path().to_path_buf()).unwrap();
    let mut sink = RecordingSink::default();
    state
        .run_turn_with_events(
            "__ferrum_test_read_image__".to_string(),
            &config,
            TurnOptions::headless(events::TurnCancellation::new()),
            &mut sink,
        )
        .await
        .unwrap();
    assert_eq!(
        image_totals(
            state
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
        )
        .0,
        0
    );
    assert!(sink.events.iter().any(|event| matches!(event,
        AgentEvent::ToolCallCompleted { is_error: true, content, .. } if content.contains("does not support image input"))));
}

fn result_with_image() -> ExecutedToolUse {
    ExecutedToolUse {
        id: "image".into(),
        name: "read".into(),
        input: serde_json::json!({"path":"image.png"}),
        content: "image attached".into(),
        images: vec![messages::ContentBlock::Image {
            mime_type: "image/png".into(),
            data_base64: "AAAA".into(),
            sha256: "hash".into(),
            source: "image.png".into(),
        }],
        is_error: false,
        aborted: false,
        duration_ms: 0,
    }
}

#[test]
fn read_image_budget_counts_attachments_all_rounds_and_session_history() {
    let initial = result_with_image().images;
    let mut budget = ToolImageBudget::new(true, &initial);
    budget.retained = image_totals(initial.iter());
    for _ in 1..MAX_IMAGES_PER_TURN {
        let mut result = result_with_image();
        budget.accept(&mut result);
        assert!(!result.is_error);
    }
    let mut overflow = result_with_image();
    budget.accept(&mut overflow);
    assert!(overflow.is_error);
    assert!(overflow.content.contains("per-turn limit"));
    assert!(overflow.images.is_empty());
    assert_eq!(budget.turn.0, MAX_IMAGES_PER_TURN);

    let mut budget = ToolImageBudget::new(true, &[]);
    budget.retained = (MAX_IMAGES_PER_SESSION, 0, 0);
    let mut result = result_with_image();
    budget.accept(&mut result);
    assert!(result.is_error);
    assert!(result.content.contains("retained-session limit"));
    assert!(result.images.is_empty());
    assert_eq!(budget.turn.0, 0);

    let mut budget = ToolImageBudget::new(true, &[]);
    budget.turn = (0, MAX_IMAGE_BYTES_PER_TURN, 0);
    let mut result = result_with_image();
    budget.accept(&mut result);
    assert!(result.is_error);
    assert!(result.content.contains("per-turn byte limit"));

    let mut budget = ToolImageBudget::new(true, &[]);
    budget.retained = (0, MAX_IMAGE_BYTES_PER_SESSION, 0);
    let mut result = result_with_image();
    budget.accept(&mut result);
    assert!(result.is_error);
    assert!(result.content.contains("retained-session byte limit"));
}

#[tokio::test]
async fn parallel_image_reads_enforce_budget_before_completion_events() {
    let root = tempfile::tempdir().unwrap();
    let config = test_config(root.path().to_path_buf());
    write_image(&root.path().join("image.png"), image::ImageFormat::Png);
    let mut state = AgentSession::new_at_cwd(&config, root.path().to_path_buf()).unwrap();
    state.active_tool_names.insert("read".to_string());
    let calls = (0..MAX_IMAGES_PER_TURN + 2)
        .map(|i| {
            (
                format!("image-{i}"),
                "read".to_string(),
                serde_json::json!({"path":"image.png"}),
            )
        })
        .collect();
    let mut sink = RecordingSink::default();
    let result = state
        .execute_tool_batch(
            calls,
            &mut ToolImageBudget::new(true, &[]),
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            &events::TurnCancellation::new(),
            &mut sink,
        )
        .await
        .unwrap();
    assert_eq!(
        result.tools.iter().filter(|tool| !tool.is_error).count(),
        MAX_IMAGES_PER_TURN
    );
    assert_eq!(
        result
            .tools
            .iter()
            .map(|tool| tool.images.len())
            .sum::<usize>(),
        MAX_IMAGES_PER_TURN
    );
    assert_eq!(
        sink.events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallCompleted { is_error: true, .. }))
            .count(),
        2
    );
    assert!(
        result.tools[MAX_IMAGES_PER_TURN..]
            .iter()
            .all(|tool| tool.images.is_empty())
    );
}
