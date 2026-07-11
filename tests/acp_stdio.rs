use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
};
use tempfile::TempDir;

struct AcpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    stderr: ChildStderr,
    _root: TempDir,
}

impl AcpProcess {
    fn spawn(cwd: &Path, fake_script: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let data = root.path().join("data");
        std::fs::create_dir(&config).unwrap();
        std::fs::create_dir(&data).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ferrum"));
        command
            .arg("acp")
            .current_dir(cwd)
            .env("FERRUM_CONFIG_DIR", config)
            .env("FERRUM_DATA_DIR", data)
            .env("FERRUM_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(script) = fake_script {
            command.env("FERRUM_FAKE_SCRIPT", script);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        Self {
            child,
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr,
            _root: root,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &message).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            self.stdout.as_mut().unwrap().read_line(&mut line).unwrap(),
            0,
            "unexpected ACP EOF"
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("ACP stdout was not protocol JSON: {error}: {line:?}"))
    }

    fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {}}
        }));
        let response = self.recv();
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], 1);
        assert_eq!(
            response["result"]["agentCapabilities"]["promptCapabilities"]["image"],
            true
        );
    }

    fn new_session(&mut self, cwd: &Path) -> String {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": cwd, "mcpServers": []}
        }));
        let response = self.recv();
        response["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn close_stdout(&mut self) {
        self.stdout.take();
    }

    fn wait_for_exit(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn finish_after_disconnect(mut self) -> String {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut self.stderr, &mut stderr).unwrap();
        assert!(
            !status.success(),
            "ACP unexpectedly succeeded after disconnect"
        );
        stderr
    }

    fn finish(mut self) -> String {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut self.stderr, &mut stderr).unwrap();
        assert!(status.success(), "ACP process failed: {status}: {stderr}");
        stderr
    }
}

impl Drop for AcpProcess {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            self.stdin.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn acp_stdio_streams_fake_provider_turn() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "smoke"}]
        }
    }));

    let mut text = String::new();
    let mut saw_usage = false;
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        assert_eq!(message["method"], "session/update");
        match message["params"]["update"]["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => text.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap(),
            ),
            Some("usage_update") => saw_usage = true,
            other => panic!("unexpected update: {other:?}"),
        }
    }
    assert_eq!(text, "fake provider response: smoke\n");
    assert!(saw_usage);
    acp.finish();
}

#[test]
fn acp_stdio_accepts_validated_image_prompt() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [
            {"type": "text", "text": "image"},
            {
                "type": "image",
                "mimeType": "image/png",
                "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            }
        ]}
    }));
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
    }
    acp.finish();
}

#[test]
fn acp_stdio_streams_sanitized_thought_summary() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("thought"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "think"}]}
    }));
    let mut thought = String::new();
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            break;
        }
        if message["params"]["update"]["sessionUpdate"] == "agent_thought_chunk" {
            thought.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap(),
            );
        }
    }
    assert_eq!(thought, "visible thought summary");
    acp.finish();
}

#[test]
fn acp_stdio_negotiates_official_v1_for_newer_client_version() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": 99, "clientCapabilities": {}}
    }));
    let response = acp.recv();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], 1);
    acp.finish();
}

#[test]
fn acp_stdio_returns_protocol_errors_without_stdout_noise() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);

    acp.send_raw("{");
    assert_eq!(acp.recv()["error"]["code"], -32700);
    acp.send(json!([]));
    assert_eq!(acp.recv()["error"]["code"], -32600);
    acp.send(json!({
        "jsonrpc": "2.0", "id": true, "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {}}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32600);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 10, "method": "session/prompt",
        "params": {"sessionId": "missing", "prompt": [{"type": "text", "text": "x"}]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);

    acp.initialize();
    acp.send(json!({"jsonrpc": "2.0", "id": 11, "method": "unknown", "params": {}}));
    assert_eq!(acp.recv()["error"]["code"], -32601);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 12, "method": "session/new",
        "params": {"cwd": "relative", "mcpServers": []}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 13, "method": "session/prompt",
        "params": {"sessionId": "missing", "prompt": [{"type": "text", "text": "x"}]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32002);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 14, "method": "session/prompt",
        "params": {"sessionId": "missing", "prompt": [false]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.finish();
}

#[test]
fn acp_stdio_rejects_duplicate_prompt_and_cancels_active_turn() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("wait_cancel"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    let prompt = |id| {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "wait"}]}
        })
    };
    acp.send(prompt(3));
    acp.send(prompt(4));
    let duplicate = acp.recv();
    assert_eq!(duplicate["id"], 4);
    assert_eq!(duplicate["error"]["code"], -32602);
    acp.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}
    }));
    let cancelled = acp.recv();
    assert_eq!(cancelled["id"], 3);
    assert_eq!(cancelled["result"]["stopReason"], "cancelled");
    acp.finish();
}

#[test]
fn acp_stdio_runs_separate_sessions_concurrently() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("wait_cancel"));
    acp.initialize();
    let waiting_session = acp.new_session(cwd.path());
    let fast_session = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": waiting_session, "prompt": [{"type": "text", "text": "wait"}]}
    }));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/prompt",
        "params": {"sessionId": fast_session, "prompt": [{"type": "text", "text": "fast"}]}
    }));
    loop {
        let message = acp.recv();
        assert_ne!(
            message["id"], 3,
            "waiting session completed before cancellation"
        );
        if message["id"] == 4 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
    }
    acp.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel",
        "params": {"sessionId": waiting_session}
    }));
    let cancelled = acp.recv();
    assert_eq!(cancelled["id"], 3);
    assert_eq!(cancelled["result"]["stopReason"], "cancelled");
    acp.finish();
}

#[test]
fn acp_stdio_maps_successful_tool_result_to_official_updates() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("relative.txt"), "tool marker\n").unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("single_read"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "read"}]}
    }));
    let mut saw_call = false;
    let mut saw_result = false;
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        match message["params"]["update"]["sessionUpdate"].as_str() {
            Some("tool_call") => saw_call = true,
            Some("tool_call_update") if message["params"]["update"]["status"] == "completed" => {
                let content = message["params"]["update"]["content"].as_array().unwrap();
                assert!(content.iter().any(|item| {
                    item["content"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("tool marker"))
                }));
                saw_result = true;
            }
            _ => {}
        }
    }
    assert!(saw_call);
    assert!(saw_result);
    acp.finish();
}

#[test]
fn acp_stdio_maps_tool_cancellation_to_official_updates() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("cancel_bash"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "run"}]}
    }));
    loop {
        let message = acp.recv();
        if message["params"]["update"]["sessionUpdate"] == "tool_call" {
            break;
        }
    }
    acp.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}
    }));
    let mut saw_failed_update = false;
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "cancelled");
            break;
        }
        if message["params"]["update"]["sessionUpdate"] == "tool_call_update"
            && message["params"]["update"]["status"] == "failed"
        {
            saw_failed_update = true;
        }
    }
    assert!(saw_failed_update);
    acp.finish();
}

#[test]
fn acp_stdio_output_disconnect_cancels_work_and_exits() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("cancel_bash"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.close_stdout();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "run"}]}
    }));
    assert!(
        acp.wait_for_exit(std::time::Duration::from_secs(3)),
        "ACP did not exit after its output disconnected"
    );
    let stderr = acp.finish_after_disconnect();
    assert!(stderr.contains("ACP output disconnected"));
}

#[test]
fn acp_stdio_maps_tool_failure_to_official_updates() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("missing_read"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "read"}]}
    }));
    let mut saw_call = false;
    let mut saw_failure = false;
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        match message["params"]["update"]["sessionUpdate"].as_str() {
            Some("tool_call") => {
                saw_call = true;
                assert_eq!(message["params"]["update"]["status"], "in_progress");
            }
            Some("tool_call_update") => {
                if message["params"]["update"]["status"] == "failed" {
                    saw_failure = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_call);
    assert!(saw_failure);
    acp.finish();
}
