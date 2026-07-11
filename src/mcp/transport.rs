use super::{DiagnosticRing, truncate_chars};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    process::{ChildStdin, ChildStdout},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};

pub(super) const MAX_MCP_FRAME_BYTES: usize = 10 * 1024 * 1024;
const MAX_MCP_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_MCP_HEADER_BYTES: usize = 64 * 1024;
const MAX_MCP_HEADERS: usize = 64;
const MAX_MCP_PROTOCOL_ERROR_CHARS: usize = 2_000;
const MCP_CHANNEL_CAPACITY: usize = 32;

struct PendingRequest {
    method: String,
    sender: oneshot::Sender<Result<Value, String>>,
}

struct WriteCommand {
    frame: Vec<u8>,
    result: Option<oneshot::Sender<Result<(), String>>>,
}

pub(super) struct McpTransport {
    writer: mpsc::Sender<WriteCommand>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    failed: Arc<AtomicBool>,
    reader_task: JoinHandle<()>,
    writer_task: JoinHandle<()>,
}

impl McpTransport {
    pub(super) fn start(
        stdin: ChildStdin,
        stdout: ChildStdout,
        diagnostics: DiagnosticRing,
    ) -> Self {
        let (writer, writer_rx) = mpsc::channel(MCP_CHANNEL_CAPACITY);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let writer_task = tokio::spawn(writer_loop(
            stdin,
            writer_rx,
            Arc::clone(&failed),
            Arc::clone(&pending),
        ));
        let reader_task = tokio::spawn(reader_loop(
            stdout,
            writer.clone(),
            Arc::clone(&pending),
            Arc::clone(&failed),
            diagnostics,
        ));
        Self {
            writer,
            pending,
            failed,
            reader_task,
            writer_task,
        }
    }

    pub(super) async fn request(
        &self,
        id: u64,
        method: &str,
        message: &Value,
        queued: &AtomicBool,
    ) -> Result<Value> {
        self.ensure_healthy()?;
        let frame = encode_message_line(message)?;
        let (response_tx, response_rx) = oneshot::channel();
        let previous = self.pending.lock().await.insert(
            id,
            PendingRequest {
                method: method.to_string(),
                sender: response_tx,
            },
        );
        if previous.is_some() {
            anyhow::bail!("duplicate MCP request id {id}");
        }

        if let Err(error) = self.write_frame(frame, Some(queued)).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }

        match response_rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(anyhow::anyhow!(error)),
            Err(_) => {
                self.ensure_healthy()?;
                anyhow::bail!("MCP response channel closed for {method}")
            }
        }
    }

    pub(super) async fn notification(&self, message: &Value) -> Result<()> {
        self.ensure_healthy()?;
        self.write_frame(encode_message_line(message)?, None).await
    }

    pub(super) async fn abandon(&self, id: u64) {
        self.pending.lock().await.remove(&id);
    }

    fn ensure_healthy(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            anyhow::bail!("MCP transport is unavailable after a protocol or I/O failure");
        }
        Ok(())
    }

    async fn write_frame(&self, frame: Vec<u8>, queued: Option<&AtomicBool>) -> Result<()> {
        let (result_tx, result_rx) = oneshot::channel();
        self.writer
            .send(WriteCommand {
                frame,
                result: Some(result_tx),
            })
            .await
            .map_err(|_| anyhow::anyhow!("MCP writer task is unavailable"))?;
        if let Some(queued) = queued {
            queued.store(true, Ordering::Release);
        }
        match result_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(anyhow::anyhow!(error)),
            Err(_) => anyhow::bail!("MCP writer task stopped before confirming the frame"),
        }
    }
}

impl Drop for McpTransport {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.writer_task.abort();
    }
}

async fn writer_loop<W>(
    mut stdin: W,
    mut commands: mpsc::Receiver<WriteCommand>,
    failed: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(command) = commands.recv().await {
        let result: Result<()> = async {
            stdin
                .write_all(&command.frame)
                .await
                .context("failed to write MCP frame")?;
            stdin.flush().await.context("failed to flush MCP frame")?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                if let Some(sender) = command.result {
                    let _ = sender.send(Ok(()));
                }
            }
            Err(error) => {
                let message = error.to_string();
                failed.store(true, Ordering::Release);
                if let Some(sender) = command.result {
                    let _ = sender.send(Err(message.clone()));
                }
                fail_pending(&pending, &message).await;
                while let Ok(command) = commands.try_recv() {
                    if let Some(sender) = command.result {
                        let _ = sender.send(Err(message.clone()));
                    }
                }
                return;
            }
        }
    }
}

async fn reader_loop(
    stdout: ChildStdout,
    writer: mpsc::Sender<WriteCommand>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    failed: Arc<AtomicBool>,
    diagnostics: DiagnosticRing,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let message = match read_message(&mut reader).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                failed.store(true, Ordering::Release);
                fail_pending(&pending, "MCP server closed stdout").await;
                return;
            }
            Err(error) => {
                failed.store(true, Ordering::Release);
                diagnostics
                    .push(format!("MCP transport read failed: {error}"))
                    .await;
                fail_pending(&pending, &format!("MCP transport read failed: {error}")).await;
                return;
            }
        };
        route_message(message, &writer, &pending, &diagnostics).await;
    }
}

async fn route_message(
    message: Value,
    writer: &mpsc::Sender<WriteCommand>,
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    diagnostics: &DiagnosticRing,
) {
    let Some(object) = message.as_object() else {
        diagnostics
            .push("ignored MCP message that was not a JSON object")
            .await;
        return;
    };

    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        reject_matching_response(object, pending, "MCP response missing jsonrpc 2.0").await;
        diagnostics
            .push("ignored MCP message with unsupported JSON-RPC version")
            .await;
        return;
    }

    if object.get("method").and_then(Value::as_str).is_some() {
        if let Some(id) = object.get("id").cloned() {
            queue_server_request_rejection(writer, id);
            diagnostics
                .push("rejected unsupported MCP server request")
                .await;
        } else {
            diagnostics.push("received MCP notification").await;
        }
        return;
    }

    let Some(id) = object.get("id").and_then(Value::as_u64) else {
        diagnostics
            .push("ignored MCP response without an unsigned integer id")
            .await;
        return;
    };
    let result = object.get("result");
    let error = object.get("error");
    let response = match (result, error) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) if valid_json_rpc_error(error) => Err(format!(
            "MCP request failed: {}",
            truncate_chars(&error.to_string(), MAX_MCP_PROTOCOL_ERROR_CHARS)
        )),
        (None, Some(_)) => Err("MCP response contains an invalid error object".to_string()),
        _ => Err("MCP response must contain exactly one of result or error".to_string()),
    };

    let request = pending.lock().await.remove(&id);
    if let Some(request) = request {
        let response = response.map_err(|error| format!("MCP {} failed: {error}", request.method));
        let _ = request.sender.send(response);
    } else {
        diagnostics
            .push(format!("ignored response for unknown MCP request id {id}"))
            .await;
    }
}

fn valid_json_rpc_error(error: &Value) -> bool {
    error.get("code").and_then(Value::as_i64).is_some()
        && error.get("message").and_then(Value::as_str).is_some()
}

async fn reject_matching_response(
    object: &Map<String, Value>,
    pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
    error: &str,
) {
    let Some(id) = object.get("id").and_then(Value::as_u64) else {
        return;
    };
    if let Some(request) = pending.lock().await.remove(&id) {
        let _ = request
            .sender
            .send(Err(format!("MCP {} failed: {error}", request.method)));
    }
}

fn queue_server_request_rejection(writer: &mpsc::Sender<WriteCommand>, id: Value) {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": "Ferrum does not support server-originated MCP requests"
        }
    });
    let Ok(frame) = encode_message_line(&message) else {
        return;
    };
    let _ = writer.try_send(WriteCommand {
        frame,
        result: None,
    });
}

async fn fail_pending(pending: &Arc<Mutex<HashMap<u64, PendingRequest>>>, error: &str) {
    let requests = std::mem::take(&mut *pending.lock().await);
    for (_, request) in requests {
        let _ = request.sender.send(Err(error.to_string()));
    }
}

fn encode_message_line(message: &Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec(message)?;
    if body.len() > MAX_MCP_FRAME_BYTES {
        anyhow::bail!(
            "outbound MCP frame is {} bytes, exceeding limit {}",
            body.len(),
            MAX_MCP_FRAME_BYTES
        );
    }
    body.push(b'\n');
    Ok(body)
}

async fn read_message<R>(reader: &mut R) -> Result<Option<Value>>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let (first_line, json_line) = loop {
        let Some((first_line, json_line)) = read_initial_line(reader).await? else {
            return Ok(None);
        };
        if !trim_line_ending(&first_line)
            .iter()
            .all(u8::is_ascii_whitespace)
        {
            break (first_line, json_line);
        }
    };
    if json_line {
        let trimmed = trim_line_ending(&first_line);
        return serde_json::from_slice(trimmed)
            .context("failed to parse MCP JSON-RPC line")
            .map(Some);
    }

    let mut header_bytes = first_line.len();
    let mut header_count = 1usize;
    let mut headers = vec![line_to_header(&first_line)?];
    loop {
        let line = read_bounded_line(reader, MAX_MCP_HEADER_LINE_BYTES)
            .await?
            .context("MCP server closed stdout before framed body")?;
        header_bytes = header_bytes
            .checked_add(line.len())
            .context("MCP header byte count overflow")?;
        if header_bytes > MAX_MCP_HEADER_BYTES {
            anyhow::bail!("MCP headers exceed {MAX_MCP_HEADER_BYTES} bytes");
        }
        if trim_line_ending(&line).is_empty() {
            break;
        }
        header_count += 1;
        if header_count > MAX_MCP_HEADERS {
            anyhow::bail!("MCP message has more than {MAX_MCP_HEADERS} headers");
        }
        headers.push(line_to_header(&line)?);
    }

    let length = parse_content_length_headers(&headers)?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .context("MCP server closed stdout during framed body")?;
    serde_json::from_slice(&body)
        .context("failed to parse framed MCP JSON-RPC message")
        .map(Some)
}

async fn read_initial_line<R>(reader: &mut R) -> Result<Option<(Vec<u8>, bool)>>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    let mut json_line = None;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some((output, json_line.unwrap_or(false))))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        output.extend_from_slice(&available[..take]);
        reader.consume(take);

        if json_line.is_none()
            && let Some(byte) = output
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
        {
            json_line = Some(matches!(byte, b'{' | b'['));
        }
        if json_line == Some(true) {
            let payload_len = output
                .len()
                .saturating_sub(usize::from(output.last() == Some(&b'\n')));
            if payload_len > MAX_MCP_FRAME_BYTES {
                anyhow::bail!("MCP stdout JSON line exceeds {MAX_MCP_FRAME_BYTES} bytes");
            }
        } else if output.len() > MAX_MCP_HEADER_LINE_BYTES {
            anyhow::bail!("MCP stdout header line exceeds {MAX_MCP_HEADER_LINE_BYTES} bytes");
        }
        if output.last() == Some(&b'\n') {
            return Ok(Some((output, json_line.unwrap_or(false))));
        }
    }
}

async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some(output))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(take) > limit {
            anyhow::bail!("MCP line exceeds {limit} bytes");
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if output.last() == Some(&b'\n') {
            return Ok(Some(output));
        }
    }
}

fn line_to_header(line: &[u8]) -> Result<String> {
    std::str::from_utf8(trim_line_ending(line))
        .context("MCP header is not valid UTF-8")
        .map(str::to_string)
}

fn trim_line_ending(mut line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    line
}

fn parse_content_length_headers(headers: &[String]) -> Result<usize> {
    let mut length = None;
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            anyhow::bail!("invalid MCP header syntax");
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                anyhow::bail!("MCP framed message has multiple Content-Length headers");
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid MCP Content-Length")?,
            );
        }
    }
    let length = length.context("MCP framed message missing Content-Length")?;
    if length > MAX_MCP_FRAME_BYTES {
        anyhow::bail!("MCP message Content-Length exceeds {MAX_MCP_FRAME_BYTES} bytes");
    }
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    #[tokio::test]
    async fn reads_json_line_at_frame_limit() {
        let body = format!(r#"{{"value":"{}"}}"#, "x".repeat(MAX_MCP_FRAME_BYTES - 12));
        assert_eq!(body.len(), MAX_MCP_FRAME_BYTES);
        let (mut writer, reader) = duplex(MAX_MCP_FRAME_BYTES + 1);
        let send = tokio::spawn(async move {
            writer.write_all(body.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        });
        let value = read_message(&mut BufReader::new(reader))
            .await
            .unwrap()
            .unwrap();
        send.await.unwrap();
        assert_eq!(
            value["value"].as_str().unwrap().len(),
            MAX_MCP_FRAME_BYTES - 12
        );
    }

    #[tokio::test]
    async fn rejects_unbounded_json_line() {
        let (mut writer, reader) = duplex(MAX_MCP_FRAME_BYTES + 2);
        writer.write_all(b"{").await.unwrap();
        writer
            .write_all(&vec![b'x'; MAX_MCP_FRAME_BYTES])
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        let error = read_message(&mut BufReader::new(reader)).await.unwrap_err();
        assert!(error.to_string().contains("JSON line exceeds"));
    }

    #[tokio::test]
    async fn rejects_unbounded_header_line() {
        let (mut writer, reader) = duplex(MAX_MCP_HEADER_LINE_BYTES * 2);
        writer
            .write_all(&vec![b'A'; MAX_MCP_HEADER_LINE_BYTES + 1])
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        let error = read_message(&mut BufReader::new(reader)).await.unwrap_err();
        assert!(error.to_string().contains("line exceeds"));
    }

    #[tokio::test]
    async fn rejects_aggregate_headers() {
        let header = format!("X-Test: {}\r\n", "x".repeat(MAX_MCP_HEADER_LINE_BYTES - 12));
        let input = header.repeat(9);
        let (mut writer, reader) = duplex(input.len());
        writer.write_all(input.as_bytes()).await.unwrap();
        writer.shutdown().await.unwrap();
        let error = read_message(&mut BufReader::new(reader)).await.unwrap_err();
        assert!(error.to_string().contains("headers exceed"));
    }

    #[tokio::test]
    async fn rejects_oversized_content_length_before_body_allocation() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_MCP_FRAME_BYTES + 1);
        let (mut writer, reader) = duplex(input.len());
        writer.write_all(input.as_bytes()).await.unwrap();
        let error = read_message(&mut BufReader::new(reader)).await.unwrap_err();
        assert!(error.to_string().contains("Content-Length exceeds"));
    }

    #[tokio::test]
    async fn accepts_lowercase_content_length_and_extra_headers() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let input = format!(
            "X-Test: before\r\ncontent-length: {}\r\nAnother: after\r\n\r\n",
            body.len()
        );
        let (mut writer, reader) = duplex(input.len() + body.len());
        writer.write_all(input.as_bytes()).await.unwrap();
        writer.write_all(body).await.unwrap();
        let value = read_message(&mut BufReader::new(reader))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["id"], 1);
    }

    #[tokio::test]
    async fn accepts_newline_between_framed_and_jsonl_messages() {
        let framed = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let jsonl = b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n";
        let header = format!("Content-Length: {}\r\n\r\n", framed.len());
        let (mut writer, reader) = duplex(header.len() + framed.len() + jsonl.len() + 1);
        writer.write_all(header.as_bytes()).await.unwrap();
        writer.write_all(framed).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.write_all(jsonl).await.unwrap();
        let mut reader = BufReader::new(reader);
        assert_eq!(read_message(&mut reader).await.unwrap().unwrap()["id"], 1);
        assert_eq!(read_message(&mut reader).await.unwrap().unwrap()["id"], 2);
    }

    #[tokio::test]
    async fn rejects_duplicate_content_length() {
        let input = "Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let (mut writer, reader) = duplex(input.len());
        writer.write_all(input.as_bytes()).await.unwrap();
        let error = read_message(&mut BufReader::new(reader)).await.unwrap_err();
        assert!(error.to_string().contains("multiple Content-Length"));
    }

    #[tokio::test]
    async fn routes_invalid_matching_envelope_as_error() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = oneshot::channel();
        pending.lock().await.insert(
            7,
            PendingRequest {
                method: "tools/call".to_string(),
                sender,
            },
        );
        let (writer, _reader) = mpsc::channel(1);
        route_message(
            json!({"jsonrpc":"2.0","id":7}),
            &writer,
            &pending,
            &DiagnosticRing::default(),
        )
        .await;
        let error = receiver.await.unwrap().unwrap_err();
        assert!(error.contains("exactly one of result or error"));
    }

    #[tokio::test]
    async fn routes_invalid_jsonrpc_version_as_error() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (sender, receiver) = oneshot::channel();
        pending.lock().await.insert(
            8,
            PendingRequest {
                method: "initialize".to_string(),
                sender,
            },
        );
        let (writer, _reader) = mpsc::channel(1);
        route_message(
            json!({"jsonrpc":"1.0","id":8,"result":{}}),
            &writer,
            &pending,
            &DiagnosticRing::default(),
        )
        .await;
        let error = receiver.await.unwrap().unwrap_err();
        assert!(error.contains("missing jsonrpc 2.0"));
    }

    #[tokio::test]
    async fn rejects_server_originated_request() {
        let (writer, mut reader) = mpsc::channel(1);
        route_message(
            json!({"jsonrpc":"2.0","id":"server-1","method":"sampling/createMessage"}),
            &writer,
            &Arc::new(Mutex::new(HashMap::new())),
            &DiagnosticRing::default(),
        )
        .await;
        let command = reader.recv().await.unwrap();
        let response: Value = serde_json::from_slice(trim_line_ending(&command.frame)).unwrap();
        assert_eq!(response["id"], "server-1");
        assert_eq!(response["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn dropped_write_waiter_does_not_corrupt_following_frame() {
        let (client, mut server) = duplex(8);
        let (sender, receiver) = mpsc::channel(4);
        let failed = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let writer = tokio::spawn(writer_loop(client, receiver, failed, pending));

        let (first_tx, first_rx) = oneshot::channel();
        sender
            .send(WriteCommand {
                frame: vec![b'a'; 32],
                result: Some(first_tx),
            })
            .await
            .unwrap();
        drop(first_rx);
        let (second_tx, second_rx) = oneshot::channel();
        sender
            .send(WriteCommand {
                frame: b"second\n".to_vec(),
                result: Some(second_tx),
            })
            .await
            .unwrap();

        let mut received = vec![0; 39];
        server.read_exact(&mut received).await.unwrap();
        second_rx.await.unwrap().unwrap();
        assert_eq!(&received[..32], vec![b'a'; 32]);
        assert_eq!(&received[32..], b"second\n");

        drop(sender);
        writer.await.unwrap();
    }

    #[test]
    fn outbound_frame_is_bounded() {
        let message = json!({"value":"x".repeat(MAX_MCP_FRAME_BYTES)});
        let error = encode_message_line(&message).unwrap_err();
        assert!(error.to_string().contains("outbound MCP frame"));
    }
}
