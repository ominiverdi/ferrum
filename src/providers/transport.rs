use crate::cancel::{self, WaitError};
use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt, pin_mut};
use reqwest::Response;
use std::{
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

pub(crate) const MAX_PROVIDER_ERROR_BODY_BYTES: usize = 256 * 1024;
pub(crate) const MAX_PROVIDER_JSON_BODY_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_MODEL_LIST_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SSE_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct BodyLimits {
    pub max_bytes: usize,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
}

pub(crate) async fn collect_response_body(
    response: Response,
    cancelled: Option<&Arc<AtomicBool>>,
    limits: BodyLimits,
    label: &str,
) -> Result<Vec<u8>> {
    let stream = response.bytes_stream();
    pin_mut!(stream);
    collect_stream(stream.as_mut(), cancelled, limits, label).await
}

async fn collect_stream<S, B, E>(
    mut stream: Pin<&mut S>,
    cancelled: Option<&Arc<AtomicBool>>,
    limits: BodyLimits,
    label: &str,
) -> Result<Vec<u8>>
where
    S: Stream<Item = std::result::Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::error::Error + Send + Sync + 'static,
{
    let started = Instant::now();
    let mut body = Vec::new();
    loop {
        let remaining = limits.total_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            anyhow::bail!(
                "{label} exceeded its {}s total body deadline",
                limits.total_timeout.as_secs_f64()
            );
        }
        let wait = limits.idle_timeout.min(remaining);
        let next = cancel::race_timeout(stream.next(), cancelled, wait).await;
        let chunk = match next {
            Ok(Some(chunk)) => chunk.context(format!("failed to read {label}"))?,
            Ok(None) => return Ok(body),
            Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
            Err(WaitError::TimedOut) if started.elapsed() >= limits.total_timeout => {
                anyhow::bail!(
                    "{label} exceeded its {}s total body deadline",
                    limits.total_timeout.as_secs_f64()
                )
            }
            Err(WaitError::TimedOut) => anyhow::bail!(
                "{label} was idle for {}s while reading the body",
                limits.idle_timeout.as_secs_f64()
            ),
        };
        let chunk = chunk.as_ref();
        if body.len().saturating_add(chunk.len()) > limits.max_bytes {
            anyhow::bail!("{label} exceeded {} bytes", limits.max_bytes);
        }
        body.extend_from_slice(chunk);
    }
}

#[derive(Debug)]
pub(crate) enum SseControl {
    Continue,
    Stop,
}

pub(crate) async fn consume_sse_response(
    response: Response,
    cancelled: Option<&Arc<AtomicBool>>,
    idle_timeout: Duration,
    label: &str,
    mut on_data: impl FnMut(&str) -> Result<SseControl>,
) -> Result<()> {
    let stream = response.bytes_stream();
    pin_mut!(stream);
    consume_sse_stream(
        stream.as_mut(),
        cancelled,
        idle_timeout,
        label,
        &mut on_data,
    )
    .await
}

async fn consume_sse_stream<S, B, E>(
    mut stream: Pin<&mut S>,
    cancelled: Option<&Arc<AtomicBool>>,
    idle_timeout: Duration,
    label: &str,
    on_data: &mut impl FnMut(&str) -> Result<SseControl>,
) -> Result<()>
where
    S: Stream<Item = std::result::Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut decoder = SseDecoder::default();
    let mut total_bytes = 0usize;
    loop {
        let next = cancel::race_timeout(stream.next(), cancelled, idle_timeout).await;
        let chunk = match next {
            Ok(Some(chunk)) => chunk.context(format!("failed to read {label}"))?,
            Ok(None) => {
                decoder.finish(on_data)?;
                return Ok(());
            }
            Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
            Err(WaitError::TimedOut) => {
                anyhow::bail!("{label} was idle for {}s", idle_timeout.as_secs_f64())
            }
        };
        let chunk = chunk.as_ref();
        total_bytes = total_bytes.saturating_add(chunk.len());
        if total_bytes > MAX_SSE_RESPONSE_BYTES {
            anyhow::bail!("{label} exceeded {MAX_SSE_RESPONSE_BYTES} bytes");
        }
        if matches!(decoder.push(chunk, on_data)?, SseControl::Stop) {
            return Ok(());
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<u8>,
    event_bytes: usize,
    saw_data: bool,
    first_line: bool,
    skip_lf: bool,
}

impl SseDecoder {
    fn push(
        &mut self,
        chunk: &[u8],
        on_data: &mut impl FnMut(&str) -> Result<SseControl>,
    ) -> Result<SseControl> {
        for byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if *byte == b'\n' {
                    continue;
                }
            }
            match *byte {
                b'\r' => {
                    if matches!(self.finish_line(on_data)?, SseControl::Stop) {
                        return Ok(SseControl::Stop);
                    }
                    self.skip_lf = true;
                }
                b'\n' => {
                    if matches!(self.finish_line(on_data)?, SseControl::Stop) {
                        return Ok(SseControl::Stop);
                    }
                }
                byte => {
                    if self.pending.len() >= MAX_SSE_LINE_BYTES {
                        anyhow::bail!("SSE line exceeded {MAX_SSE_LINE_BYTES} bytes");
                    }
                    self.pending.push(byte);
                }
            }
        }
        Ok(SseControl::Continue)
    }

    fn finish(&mut self, on_data: &mut impl FnMut(&str) -> Result<SseControl>) -> Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.process_line(&line, on_data)?;
        }
        if self.saw_data || self.event_bytes > 0 {
            anyhow::bail!("SSE stream ended before the current event was terminated");
        }
        Ok(())
    }

    fn finish_line(
        &mut self,
        on_data: &mut impl FnMut(&str) -> Result<SseControl>,
    ) -> Result<SseControl> {
        let line = std::mem::take(&mut self.pending);
        self.process_line(&line, on_data)
    }

    fn process_line(
        &mut self,
        raw_line: &[u8],
        on_data: &mut impl FnMut(&str) -> Result<SseControl>,
    ) -> Result<SseControl> {
        let line = if !self.first_line {
            self.first_line = true;
            raw_line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw_line)
        } else {
            raw_line
        };
        if line.len() > MAX_SSE_LINE_BYTES {
            anyhow::bail!("SSE line exceeded {MAX_SSE_LINE_BYTES} bytes");
        }
        if line.is_empty() {
            return self.dispatch(on_data);
        }
        self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
        if self.event_bytes > MAX_SSE_EVENT_BYTES {
            anyhow::bail!("SSE event exceeded {MAX_SSE_EVENT_BYTES} bytes");
        }
        let line = std::str::from_utf8(line).context("SSE stream contained invalid UTF-8")?;
        if line.starts_with(':') {
            return Ok(SseControl::Continue);
        }
        let (field, mut value) = line.split_once(':').unwrap_or((line, ""));
        if let Some(stripped) = value.strip_prefix(' ') {
            value = stripped;
        }
        if field == "data" {
            if self.saw_data {
                self.data.push(b'\n');
            }
            if self.data.len().saturating_add(value.len()) > MAX_SSE_EVENT_BYTES {
                anyhow::bail!("SSE data field exceeded {MAX_SSE_EVENT_BYTES} bytes");
            }
            self.data.extend_from_slice(value.as_bytes());
            self.saw_data = true;
        }
        Ok(SseControl::Continue)
    }

    fn dispatch(
        &mut self,
        on_data: &mut impl FnMut(&str) -> Result<SseControl>,
    ) -> Result<SseControl> {
        self.event_bytes = 0;
        if !self.saw_data {
            return Ok(SseControl::Continue);
        }
        self.saw_data = false;
        let data = std::mem::take(&mut self.data);
        let data = std::str::from_utf8(&data).context("SSE data contained invalid UTF-8")?;
        on_data(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::{convert::Infallible, io};

    fn ignore_data(_: &str) -> Result<SseControl> {
        Ok(SseControl::Continue)
    }

    #[test]
    fn decodes_utf8_split_across_chunks_and_standard_data_syntax() {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        let mut collect = |data: &str| {
            events.push(data.to_string());
            Ok(SseControl::Continue)
        };
        let bytes = "data:café\n\n".as_bytes();
        let split = bytes.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
        decoder.push(&bytes[..split], &mut collect).unwrap();
        decoder.push(&bytes[split..], &mut collect).unwrap();
        assert_eq!(events, vec!["café"]);
    }

    #[test]
    fn supports_cr_framing_and_multiline_data() {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        let mut collect = |data: &str| {
            events.push(data.to_string());
            Ok(SseControl::Continue)
        };
        decoder
            .push(b"data: first\rdata:second\r\r", &mut collect)
            .unwrap();
        assert_eq!(events, vec!["first\nsecond"]);
    }

    #[test]
    fn stops_decoding_when_consumer_accepts_done() {
        let mut decoder = SseDecoder::default();
        let mut events = 0usize;
        let mut stop = |data: &str| {
            events += 1;
            assert_eq!(data, "[DONE]");
            Ok(SseControl::Stop)
        };
        let control = decoder
            .push(b"data:[DONE]\n\ndata:not-read\n\n", &mut stop)
            .unwrap();
        assert!(matches!(control, SseControl::Stop));
        assert_eq!(events, 1);
    }

    #[test]
    fn rejects_endless_sse_line() {
        let mut decoder = SseDecoder::default();
        let mut ignore = ignore_data;
        let error = decoder
            .push(&vec![b'x'; MAX_SSE_LINE_BYTES + 1], &mut ignore)
            .unwrap_err();
        assert!(error.to_string().contains("line exceeded"));
    }

    #[test]
    fn rejects_unterminated_event_at_eof() {
        let mut decoder = SseDecoder::default();
        let mut ignore = ignore_data;
        decoder.push(b"data: {}\n", &mut ignore).unwrap();
        let error = decoder.finish(&mut ignore).unwrap_err();
        assert!(error.to_string().contains("before the current event"));
    }

    #[tokio::test]
    async fn healthy_stream_can_outlive_many_idle_windows() {
        let source = stream::unfold(0usize, |index| async move {
            if index == 12 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            Some((Ok::<_, io::Error>(b": heartbeat\n\n".to_vec()), index + 1))
        });
        pin_mut!(source);
        let mut ignore = ignore_data;
        consume_sse_stream(
            source.as_mut(),
            None,
            Duration::from_millis(20),
            "test stream",
            &mut ignore,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn dripping_body_hits_total_deadline_even_when_not_idle() {
        let source = stream::unfold(0usize, |index| async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((Ok::<_, io::Error>(vec![b'x']), index + 1))
        });
        pin_mut!(source);
        let error = collect_stream(
            source.as_mut(),
            None,
            BodyLimits {
                max_bytes: 1024,
                idle_timeout: Duration::from_millis(30),
                total_timeout: Duration::from_millis(45),
            },
            "test body",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("total body deadline"));
    }

    #[tokio::test]
    async fn body_collector_enforces_byte_limit() {
        let source = stream::iter([Ok::<_, Infallible>(vec![0u8; 5])]);
        pin_mut!(source);
        let error = collect_stream(
            source.as_mut(),
            None,
            BodyLimits {
                max_bytes: 4,
                idle_timeout: Duration::from_secs(1),
                total_timeout: Duration::from_secs(1),
            },
            "test body",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("exceeded 4 bytes"));
    }
}
