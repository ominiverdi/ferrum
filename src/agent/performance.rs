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
        (seconds > 0.0).then(|| tokens as f64 / seconds)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TurnPerformance {
    provider: String,
    model: String,
    total: Duration,
    requests: Vec<ModelPerformance>,
    context: Option<ContextSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextSnapshot {
    pub(super) tokens: usize,
    pub(super) max: usize,
    pub(super) estimated: bool,
    pub(super) source: &'static str,
}

impl ContextSnapshot {
    fn percent(&self) -> usize {
        super::context_usage_percent(self.tokens, self.max)
    }

    fn compact(&self) -> String {
        let prefix = if self.estimated { "~" } else { "" };
        format!(
            "ctx {prefix}{}/{} {}%",
            format_compact_token_count(self.tokens),
            format_compact_token_count(self.max),
            self.percent()
        )
    }

    fn detailed(&self) -> String {
        let prefix = if self.estimated { "~" } else { "" };
        format!(
            "{prefix}{}/{} ({}%, {})",
            format_token_count(self.tokens as u64),
            format_token_count(self.max as u64),
            self.percent(),
            self.source
        )
    }
}

impl TurnPerformance {
    pub(super) fn with_context(mut self, context: ContextSnapshot) -> Self {
        self.context = Some(context);
        self
    }

    pub(super) fn compact_summary(&self) -> String {
        let request_label = if self.requests.len() == 1 {
            "request"
        } else {
            "requests"
        };
        let mut output = format!("perf: {} {request_label}", self.requests.len());
        if !self.requests.is_empty() {
            let _ = write!(
                output,
                " | TTFT {} | {} | tot {}",
                format_ttft_summary(self.ttft_summary().as_ref()),
                self.throughput_summary().map_or_else(
                    || "n/a t/s".to_string(),
                    |(summary, estimated)| format_throughput_summary(&summary, estimated),
                ),
                format_total_output_tokens(&self.requests),
            );
        }
        if let Some(context) = &self.context {
            let _ = write!(output, " | {}", context.compact());
        }
        let duration = if self.total < Duration::from_secs(1) {
            format_duration(self.total)
        } else {
            format!("{:.1}s", self.total.as_secs_f64())
        };
        let _ = write!(output, " | turn {duration}");
        output
    }

    pub(super) fn detailed_summary(&self) -> String {
        let mut output = format!(
            "performance:\n  provider: {}\n  model: {}",
            self.provider, self.model
        );
        if let Some(context) = &self.context {
            let _ = write!(output, "\n  context: {}", context.detailed());
        }
        let _ = write!(
            output,
            "\n  turn: {}\n  completed_requests: {}",
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
        let completed = self
            .requests
            .iter()
            .filter(|request| !request.interrupted)
            .count();
        let ttft = self.ttft_summary();
        let throughput = self.throughput_summary();
        let _ = write!(
            output,
            "\n  tot: {}\n  TTFT: {} ({}/{completed} measured)\n  {} ({}/{completed} measured)\n  timing order: low/avg/high; t/s avg is generation-time weighted",
            format_total_output_tokens(&self.requests),
            format_ttft_summary(ttft.as_ref()),
            ttft.as_ref().map_or(0, |summary| summary.samples),
            throughput.as_ref().map_or_else(
                || "n/a t/s".to_string(),
                |(summary, estimated)| format_throughput_summary(summary, *estimated),
            ),
            throughput
                .as_ref()
                .map_or(0, |(summary, _)| summary.samples),
        );
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
                        format!("~{}", format_token_count(tokens))
                    } else {
                        format_token_count(tokens)
                    }
                },
            );
            let _ = write!(
                output,
                "\n  request {} ({}): TTFT {}, generation {}, {}, tot {} tok ({}), total {}",
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

    fn ttft_summary(&self) -> Option<ValueSummary> {
        ValueSummary::from_values(
            self.requests
                .iter()
                .filter(|request| !request.interrupted)
                .filter_map(|request| request.time_to_first_output.map(|time| time.as_secs_f64())),
        )
    }

    fn throughput_summary(&self) -> Option<(ValueSummary, bool)> {
        let mut tokens = 0.0;
        let mut seconds = 0.0;
        let mut estimated = false;
        let mut summary = ValueSummary::from_values(
            self.requests
                .iter()
                .filter(|request| !request.interrupted)
                .filter_map(|request| {
                    let rate = request.tokens_per_second()?;
                    tokens += request.output_tokens? as f64;
                    seconds += request.generation_duration()?.as_secs_f64();
                    estimated |= request.estimated_tokens;
                    Some(rate)
                }),
        )?;
        // Use exactly the requests contributing a valid rate, not token totals
        // from buffered or interrupted requests without matching generation time.
        summary.average = tokens / seconds;
        Some((summary, estimated))
    }
}

#[derive(Debug)]
struct ValueSummary {
    samples: usize,
    low: f64,
    average: f64,
    high: f64,
}

impl ValueSummary {
    fn from_values(mut values: impl Iterator<Item = f64>) -> Option<Self> {
        let first = values.next()?;
        let (mut samples, mut low, mut total, mut high) = (1, first, first, first);
        for value in values {
            samples += 1;
            low = low.min(value);
            total += value;
            high = high.max(value);
        }
        Some(Self {
            samples,
            low,
            average: total / samples as f64,
            high,
        })
    }
}

fn format_ttft_summary(summary: Option<&ValueSummary>) -> String {
    let Some(summary) = summary else {
        return "n/a".to_string();
    };
    if summary.high < 1.0 {
        if summary.samples == 1 {
            format!("{:.0}ms", summary.average * 1_000.0)
        } else {
            format!(
                "{:.0}/{:.0}/{:.0}ms",
                summary.low * 1_000.0,
                summary.average * 1_000.0,
                summary.high * 1_000.0
            )
        }
    } else if summary.samples == 1 {
        format!("{:.2}s", summary.average)
    } else {
        format!(
            "{:.2}/{:.2}/{:.2}s",
            summary.low, summary.average, summary.high
        )
    }
}

fn format_throughput_summary(summary: &ValueSummary, estimated: bool) -> String {
    let prefix = if estimated { "~" } else { "" };
    if summary.samples == 1 {
        format!("{prefix}{:.1} t/s", summary.average)
    } else {
        format!(
            "{prefix}{:.1}/{:.1}/{:.1} t/s",
            summary.low, summary.average, summary.high
        )
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
            context: None,
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
        || "n/a t/s".to_string(),
        |value| {
            let prefix = if estimated { "~" } else { "" };
            format!("{prefix}{value:.1} t/s")
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
    format!("{prefix}{} tok", format_token_count(tokens))
}

fn format_token_count(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(digit);
    }
    output
}

fn format_compact_token_count(tokens: usize) -> String {
    if tokens < 10_000 {
        return format_token_count(tokens as u64);
    }
    let kilo = tokens.div_euclid(1_000) + usize::from(tokens.rem_euclid(1_000) >= 500);
    format!("{kilo}k")
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
            context: None,
        };

        assert_eq!(
            performance.compact_summary(),
            "perf: 2 requests | TTFT 1.00/1.09/1.18s | 50.0/60.4/67.3 t/s | tot 302 tok | turn 21.4s"
        );
    }

    #[test]
    fn marks_estimates_and_unavailable_stream_metrics() {
        let performance = TurnPerformance {
            provider: "fake".to_string(),
            model: "fake".to_string(),
            total: Duration::from_millis(8),
            requests: vec![request(1, ModelRequestKind::Agent, 7, None, Some(12), true)],
            context: None,
        };

        assert_eq!(
            performance.compact_summary(),
            "perf: 1 request | TTFT n/a | n/a t/s | tot ~12 tok | turn 8ms"
        );
        assert!(performance.detailed_summary().contains(
            "request 1 (agent): TTFT n/a, generation n/a, n/a t/s, tot ~12 tok (estimated), total 7ms"
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
            context: None,
        };

        assert_eq!(
            performance.compact_summary(),
            "perf: 1 request | TTFT 100ms | ~20.0 t/s | tot ~20 tok | turn 2.0s"
        );
    }

    #[test]
    fn context_snapshot_appends_to_compact_and_detailed_summaries() {
        let performance = turn(vec![request(
            1,
            ModelRequestKind::Agent,
            1_000,
            Some(100),
            Some(50),
            false,
        )])
        .with_context(ContextSnapshot {
            tokens: 23_971,
            max: 256_000,
            estimated: true,
            source: "estimate_after_compaction",
        });

        assert!(
            performance
                .compact_summary()
                .contains("| ctx ~24k/256k 9% | turn")
        );
        assert!(
            performance
                .detailed_summary()
                .contains("context: ~23,971/256,000 (9%, estimate_after_compaction)")
        );
    }

    #[test]
    fn usage_informed_context_has_no_estimate_marker() {
        let performance = turn(vec![request(
            1,
            ModelRequestKind::Agent,
            1_000,
            Some(100),
            Some(50),
            false,
        )])
        .with_context(ContextSnapshot {
            tokens: 237_351,
            max: 256_000,
            estimated: false,
            source: "usage+estimate",
        });

        assert!(
            performance
                .compact_summary()
                .contains("| ctx 237k/256k 92% | turn")
        );
    }

    #[test]
    fn compact_context_counts_stay_exact_below_ten_thousand() {
        let performance = turn(vec![request(
            1,
            ModelRequestKind::Agent,
            1_000,
            Some(100),
            Some(50),
            false,
        )])
        .with_context(ContextSnapshot {
            tokens: 9_999,
            max: 10_000,
            estimated: false,
            source: "usage+estimate",
        });

        assert!(
            performance
                .compact_summary()
                .contains("| ctx 9,999/10k 99% | turn")
        );
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
            context: None,
        };
        assert!(performance.compact_summary().contains("2 requests"));
        assert!(performance.compact_summary().contains("tot ~25 tok"));
        let detail = performance.detailed_summary();
        assert!(detail.contains("completed_requests: 1"));
        assert!(detail.contains("interrupted_requests: 1"));
        assert!(detail.contains("request 1 (interrupted)"));
    }

    fn turn(requests: Vec<ModelPerformance>) -> TurnPerformance {
        TurnPerformance {
            provider: "mock".to_string(),
            model: "mock".to_string(),
            total: Duration::from_secs(10),
            requests,
            context: None,
        }
    }

    #[test]
    fn totals_all_requests_and_formats_thousands() {
        let performance = turn(vec![
            request(
                1,
                ModelRequestKind::Agent,
                2_000,
                Some(500),
                Some(1_000),
                false,
            ),
            request(
                2,
                ModelRequestKind::FinalSynthesis,
                1_500,
                Some(500),
                Some(574),
                false,
            ),
        ]);
        assert!(performance.compact_summary().contains("tot 1,574 tok"));
        assert!(performance.detailed_summary().contains("tot: 1,574 tok"));
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_000), "1,000");
        assert_eq!(format_token_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn interrupted_requests_contribute_to_total_but_not_timing_statistics() {
        let mut interrupted = request(
            1,
            ModelRequestKind::Agent,
            10_000,
            Some(9_000),
            Some(1_000),
            true,
        );
        interrupted.interrupted = true;
        let performance = turn(vec![
            interrupted,
            request(
                2,
                ModelRequestKind::Agent,
                2_000,
                Some(500),
                Some(150),
                false,
            ),
        ]);
        assert_eq!(
            performance.compact_summary(),
            "perf: 2 requests | TTFT 500ms | 100.0 t/s | tot ~1,150 tok | turn 10.0s"
        );
        let detail = performance.detailed_summary();
        assert!(detail.contains("TTFT: 500ms (1/1 measured)"));
        assert!(detail.contains("100.0 t/s (1/1 measured)"));
        assert!(detail.contains("request 1 (interrupted): TTFT 9.00s"));
    }

    #[test]
    fn excludes_unmeasured_rates_without_losing_their_tokens() {
        let performance = turn(vec![
            request(1, ModelRequestKind::Agent, 1_000, None, Some(100), true),
            request(
                2,
                ModelRequestKind::Agent,
                1_000,
                Some(100),
                Some(90),
                false,
            ),
            request(3, ModelRequestKind::Agent, 500, Some(500), Some(10), true),
        ]);
        assert_eq!(
            performance.compact_summary(),
            "perf: 3 requests | TTFT 100/300/500ms | 100.0 t/s | tot ~200 tok | turn 10.0s"
        );
        let detail = performance.detailed_summary();
        assert!(detail.contains("TTFT: 100/300/500ms (2/3 measured)"));
        assert!(detail.contains("100.0 t/s (1/3 measured)"));
        assert!(!detail.contains("NaN"));
        assert!(!detail.contains("inf"));
    }

    #[test]
    fn estimates_in_measured_rates_mark_the_whole_range() {
        let performance = turn(vec![
            request(
                1,
                ModelRequestKind::Agent,
                2_000,
                Some(1_000),
                Some(100),
                false,
            ),
            request(
                2,
                ModelRequestKind::Agent,
                3_000,
                Some(1_000),
                Some(100),
                true,
            ),
        ]);
        assert_eq!(
            performance.compact_summary(),
            "perf: 2 requests | TTFT 1.00/1.00/1.00s | ~50.0/66.7/100.0 t/s | tot ~200 tok | turn 10.0s"
        );
    }

    #[test]
    fn missing_token_usage_does_not_invent_a_total_or_rate() {
        let performance = turn(vec![
            request(
                1,
                ModelRequestKind::Agent,
                1_000,
                Some(100),
                Some(90),
                false,
            ),
            request(2, ModelRequestKind::Agent, 1_000, Some(300), None, false),
        ]);
        assert_eq!(
            performance.compact_summary(),
            "perf: 2 requests | TTFT 100/200/300ms | 100.0 t/s | tot n/a | turn 10.0s"
        );
        assert!(
            performance
                .detailed_summary()
                .contains("100.0 t/s (1/2 measured)")
        );
    }

    #[test]
    fn zero_generation_duration_is_unavailable_not_infinite() {
        for total in [0, 5, 10] {
            let performance = turn(vec![request(
                1,
                ModelRequestKind::Agent,
                total,
                Some(10),
                Some(1),
                false,
            )]);
            assert_eq!(performance.requests[0].tokens_per_second(), None);
            assert_eq!(
                performance.compact_summary(),
                "perf: 1 request | TTFT 10ms | n/a t/s | tot 1 tok | turn 10.0s"
            );
        }
    }

    #[test]
    fn all_interrupted_requests_have_no_completed_timing_statistics() {
        let mut interrupted = request(
            1,
            ModelRequestKind::Agent,
            1_000,
            Some(100),
            Some(90),
            false,
        );
        interrupted.interrupted = true;
        let performance = turn(vec![interrupted]);
        assert_eq!(
            performance.compact_summary(),
            "perf: 1 request | TTFT n/a | n/a t/s | tot 90 tok | turn 10.0s"
        );
        assert!(
            performance
                .detailed_summary()
                .contains("TTFT: n/a (0/0 measured)")
        );
    }

    #[test]
    fn no_requests_have_no_invented_metrics() {
        let performance = turn(Vec::new());
        assert_eq!(
            performance.compact_summary(),
            "perf: 0 requests | turn 10.0s"
        );
        assert!(
            performance
                .detailed_summary()
                .contains("model requests: none")
        );
    }

    #[test]
    fn reasoning_is_the_first_output_and_provider_tokens_are_not_recounted() {
        let mut timer = ModelPerformanceTimer::start(true);
        timer.observe(&StreamEvent::ThinkingDelta(
            "displayable provider reasoning".to_string(),
        ));
        let ttft = timer.time_to_first_output;
        assert!(ttft.is_some());
        timer.observe(&StreamEvent::TextDelta("answer".to_string()));
        assert_eq!(timer.time_to_first_output, ttft);
        let usage = TokenUsage {
            input_tokens: Some(20),
            output_tokens: Some(150),
            total_tokens: Some(170),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            source: "provider".to_string(),
        };
        let performance = turn(vec![timer.finish(1, ModelRequestKind::Agent, &usage)]);
        assert!(performance.compact_summary().contains("tot 150 tok"));
        assert!(!performance.compact_summary().contains('~'));
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
