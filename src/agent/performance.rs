use super::{events::ModelRequestKind, messages::TokenUsage};
use crate::providers::StreamEvent;
use std::{
    fmt::Write,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ModelPerformance {
    request: usize,
    kind: ModelRequestKind,
    total: Duration,
    time_to_first_output: Option<Duration>,
    output_tokens: Option<u64>,
    estimated_tokens: bool,
    interrupted: bool,
}

impl ModelPerformance {
    fn generation_duration(&self) -> Option<Duration> {
        self.time_to_first_output
            .map(|ttft| self.total.saturating_sub(ttft))
    }

    fn tokens_per_second(&self) -> Option<f64> {
        let tokens = self.output_tokens?;
        let seconds = self.generation_duration()?.as_secs_f64();
        (seconds > 0.0).then_some(tokens as f64 / seconds)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TurnPerformance {
    provider: String,
    model: String,
    total: Duration,
    requests: Vec<ModelPerformance>,
}

impl TurnPerformance {
    pub(super) fn compact_summary(&self) -> String {
        let request_label = if self.requests.len() == 1 {
            "request"
        } else {
            "requests"
        };
        let mut output = format!("perf: {} {request_label}", self.requests.len());
        if let Some(final_request) = self.requests.last() {
            let _ = write!(
                output,
                " | final ttft {} | final {} | output {}",
                format_optional_duration(final_request.time_to_first_output),
                format_throughput(
                    final_request.tokens_per_second(),
                    final_request.estimated_tokens,
                ),
                format_total_output_tokens(&self.requests),
            );
        }
        let _ = write!(output, " | turn {}", format_duration(self.total));
        output
    }

    pub(super) fn detailed_summary(&self) -> String {
        let mut output = format!(
            "performance:\n  provider: {}\n  model: {}\n  turn: {}\n  completed_requests: {}",
            self.provider,
            self.model,
            format_duration(self.total),
            self.requests
                .iter()
                .filter(|request| !request.interrupted)
                .count()
        );
        let interrupted = self
            .requests
            .iter()
            .filter(|request| request.interrupted)
            .count();
        if interrupted > 0 {
            let _ = write!(output, "\n  interrupted_requests: {interrupted}");
        }
        if self.requests.is_empty() {
            output.push_str("\n  model requests: none");
            return output;
        }
        for request in &self.requests {
            let source = if request.estimated_tokens {
                "estimated"
            } else {
                "provider"
            };
            let output_tokens = request.output_tokens.map_or_else(
                || "n/a".to_string(),
                |tokens| {
                    if request.estimated_tokens {
                        format!("~{tokens}")
                    } else {
                        tokens.to_string()
                    }
                },
            );
            let _ = write!(
                output,
                "\n  request {} ({}): ttft {}, generation {}, throughput {}, output_tokens {} ({}), total {}",
                request.request,
                if request.interrupted {
                    "interrupted"
                } else {
                    request_kind_label(request.kind)
                },
                format_optional_duration(request.time_to_first_output),
                format_optional_duration(request.generation_duration()),
                format_throughput(request.tokens_per_second(), request.estimated_tokens),
                output_tokens,
                source,
                format_duration(request.total),
            );
        }
        output
    }
}

#[derive(Debug)]
pub(super) struct TurnPerformanceRecorder {
    provider: String,
    model: String,
    started: Instant,
    requests: Vec<ModelPerformance>,
}

impl TurnPerformanceRecorder {
    pub(super) fn start(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            started: Instant::now(),
            requests: Vec::new(),
        }
    }

    pub(super) fn record(&mut self, performance: ModelPerformance) {
        self.requests.push(performance);
    }

    pub(super) fn finish(self) -> TurnPerformance {
        TurnPerformance {
            provider: self.provider,
            model: self.model,
            total: self.started.elapsed(),
            requests: self.requests,
        }
    }
}

#[derive(Debug)]
pub(super) struct ModelPerformanceTimer {
    started: Instant,
    capture_stream_timing: bool,
    time_to_first_output: Option<Duration>,
}

impl ModelPerformanceTimer {
    pub(super) fn start(capture_stream_timing: bool) -> Self {
        Self {
            started: Instant::now(),
            capture_stream_timing,
            time_to_first_output: None,
        }
    }

    pub(super) fn observe(&mut self, event: &StreamEvent) {
        if !self.capture_stream_timing || self.time_to_first_output.is_some() {
            return;
        }
        let has_output = match event {
            StreamEvent::ThinkingDelta(delta) | StreamEvent::TextDelta(delta) => !delta.is_empty(),
            StreamEvent::OutputActivity => true,
        };
        if has_output {
            self.time_to_first_output = Some(self.started.elapsed());
        }
    }

    pub(super) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub(super) fn finish(
        self,
        request: usize,
        kind: ModelRequestKind,
        usage: &TokenUsage,
    ) -> ModelPerformance {
        ModelPerformance {
            request,
            kind,
            total: self.started.elapsed(),
            time_to_first_output: self.time_to_first_output,
            output_tokens: usage.output_tokens,
            estimated_tokens: usage.source != "provider",
            interrupted: false,
        }
    }

    pub(super) fn finish_interrupted(
        self,
        request: usize,
        kind: ModelRequestKind,
        usage: &TokenUsage,
    ) -> ModelPerformance {
        let mut performance = self.finish(request, kind, usage);
        performance.interrupted = true;
        performance
    }
}

fn request_kind_label(kind: ModelRequestKind) -> &'static str {
    match kind {
        ModelRequestKind::Agent => "agent",
        ModelRequestKind::FinalSynthesis => "final",
    }
}

fn format_duration(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.2}s", duration.as_secs_f64())
    }
}

fn format_optional_duration(duration: Option<Duration>) -> String {
    duration.map_or_else(|| "n/a".to_string(), format_duration)
}

fn format_throughput(value: Option<f64>, estimated: bool) -> String {
    value.map_or_else(
        || "n/a".to_string(),
        |value| {
            let prefix = if estimated { "~" } else { "" };
            format!("{prefix}{value:.1} tok/s")
        },
    )
}

fn format_total_output_tokens(requests: &[ModelPerformance]) -> String {
    let tokens = requests.iter().try_fold(0u64, |total, request| {
        Some(total.saturating_add(request.output_tokens?))
    });
    let Some(tokens) = tokens else {
        return "n/a".to_string();
    };
    let prefix = if requests.iter().any(|request| request.estimated_tokens) {
        "~"
    } else {
        ""
    };
    format!("{prefix}{tokens} tok")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        request: usize,
        kind: ModelRequestKind,
        total_ms: u64,
        ttft_ms: Option<u64>,
        output_tokens: Option<u64>,
        estimated_tokens: bool,
    ) -> ModelPerformance {
        ModelPerformance {
            request,
            kind,
            total: Duration::from_millis(total_ms),
            time_to_first_output: ttft_ms.map(Duration::from_millis),
            output_tokens,
            estimated_tokens,
            interrupted: false,
        }
    }

    #[test]
    fn formats_compact_turn_summary() {
        let performance = TurnPerformance {
            provider: "openai-codex".to_string(),
            model: "gpt-5.3-codex".to_string(),
            total: Duration::from_millis(21_400),
            requests: vec![
                request(
                    1,
                    ModelRequestKind::Agent,
                    3_000,
                    Some(1_000),
                    Some(100),
                    false,
                ),
                request(
                    2,
                    ModelRequestKind::FinalSynthesis,
                    4_180,
                    Some(1_180),
                    Some(202),
                    false,
                ),
            ],
        };

        assert_eq!(
            performance.compact_summary(),
            "perf: 2 requests | final ttft 1.18s | final 67.3 tok/s | output 302 tok | turn 21.40s"
        );
    }

    #[test]
    fn marks_estimates_and_unavailable_stream_metrics() {
        let performance = TurnPerformance {
            provider: "fake".to_string(),
            model: "fake".to_string(),
            total: Duration::from_millis(8),
            requests: vec![request(1, ModelRequestKind::Agent, 7, None, Some(12), true)],
        };

        assert_eq!(
            performance.compact_summary(),
            "perf: 1 request | final ttft n/a | final n/a | output ~12 tok | turn 8ms"
        );
        assert!(performance.detailed_summary().contains(
            "request 1 (agent): ttft n/a, generation n/a, throughput n/a, output_tokens ~12 (estimated), total 7ms"
        ));
    }

    #[test]
    fn marks_estimated_throughput() {
        let performance = TurnPerformance {
            provider: "openai-compatible".to_string(),
            model: "local".to_string(),
            total: Duration::from_secs(2),
            requests: vec![request(
                1,
                ModelRequestKind::Agent,
                1_100,
                Some(100),
                Some(20),
                true,
            )],
        };

        assert!(performance.compact_summary().contains("final ~20.0 tok/s"));
    }

    #[test]
    fn interrupted_requests_count_usage_without_claiming_completion() {
        let usage = TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(20),
            total_tokens: Some(30),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            source: "estimated".to_string(),
        };
        let timer = ModelPerformanceTimer::start(true);
        let performance = TurnPerformance {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            total: Duration::from_secs(1),
            requests: vec![
                timer.finish_interrupted(1, ModelRequestKind::Agent, &usage),
                request(2, ModelRequestKind::Agent, 100, Some(10), Some(5), false),
            ],
        };
        assert!(performance.compact_summary().contains("2 requests"));
        assert!(performance.compact_summary().contains("output ~25 tok"));
        let detail = performance.detailed_summary();
        assert!(detail.contains("completed_requests: 1"));
        assert!(detail.contains("interrupted_requests: 1"));
        assert!(detail.contains("request 1 (interrupted)"));
    }

    #[test]
    fn timer_ignores_empty_deltas_and_non_streaming_activity() {
        let mut live = ModelPerformanceTimer::start(true);
        live.observe(&StreamEvent::TextDelta(String::new()));
        assert_eq!(live.time_to_first_output, None);
        live.observe(&StreamEvent::OutputActivity);
        assert!(live.time_to_first_output.is_some());

        let mut buffered = ModelPerformanceTimer::start(false);
        buffered.observe(&StreamEvent::TextDelta("answer".to_string()));
        assert_eq!(buffered.time_to_first_output, None);
    }
}
