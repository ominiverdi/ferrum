use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
};
use tempfile::TempDir;

struct AcpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    stderr: ChildStderr,
    _root: Option<TempDir>,
}

impl AcpProcess {
    fn spawn(cwd: &Path, fake_script: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path().to_path_buf();
        Self::spawn_with_storage(cwd, fake_script, &storage, Some(root), false, None, false)
    }

    fn spawn_asking(cwd: &Path, fake_script: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path().to_path_buf();
        Self::spawn_with_storage(cwd, fake_script, &storage, Some(root), true, None, false)
    }

    fn spawn_asking_no_tools(cwd: &Path, fake_script: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path().to_path_buf();
        Self::spawn_with_storage(cwd, fake_script, &storage, Some(root), true, None, true)
    }

    fn spawn_asking_with_safety(cwd: &Path, fake_script: Option<&str>, safety: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path().to_path_buf();
        Self::spawn_with_storage(
            cwd,
            fake_script,
            &storage,
            Some(root),
            true,
            Some(safety),
            false,
        )
    }

    fn spawn_in(cwd: &Path, fake_script: Option<&str>, storage: &Path) -> Self {
        Self::spawn_with_storage(cwd, fake_script, storage, None, false, None, false)
    }

    fn spawn_with_storage(
        cwd: &Path,
        fake_script: Option<&str>,
        storage: &Path,
        root: Option<TempDir>,
        ask_permissions: bool,
        safety: Option<&str>,
        no_tools: bool,
    ) -> Self {
        let config = storage.join("config");
        let data = storage.join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ferrum"));
        if let Some(safety) = safety {
            command.args(["--safety", safety]);
        }
        if no_tools {
            command.arg("--no-tools");
        }
        command.arg("acp");
        if ask_permissions {
            command.args(["--permissions", "ask"]);
        }
        command
            .current_dir(cwd)
            .env("FERRUM_CONFIG_DIR", config)
            .env("FERRUM_DATA_DIR", data)
            .env("FERRUM_OFFLINE", "1")
            .env("OPENAI_API_KEY", "provider-sentinel-must-not-reach-mcp")
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
        assert_eq!(response["result"]["agentCapabilities"]["loadSession"], true);
        assert_eq!(
            response["result"]["agentCapabilities"]["mcpCapabilities"],
            json!({"http": false, "sse": false})
        );
        for capability in ["list", "delete", "resume", "close"] {
            assert!(
                response["result"]["agentCapabilities"]["sessionCapabilities"][capability]
                    .is_object(),
                "missing session capability {capability}: {response}"
            );
        }
    }

    fn new_session(&mut self, cwd: &Path) -> String {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": cwd, "mcpServers": []}
        }));
        let response = self.recv();
        let session_id = response["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        self.assert_available_commands_update(&session_id);
        session_id
    }

    fn assert_available_commands_update(&mut self, session_id: &str) {
        let update = self.recv();
        assert_eq!(update["method"], "session/update", "{update}");
        assert_eq!(update["params"]["sessionId"], session_id, "{update}");
        assert_eq!(
            update["params"]["update"],
            json!({
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {
                        "name": "compact",
                        "description": "Compact the current session context",
                        "input": {"hint": "optional compaction instructions"}
                    },
                    {
                        "name": "session",
                        "description": "Show current session information"
                    },
                    {
                        "name": "version",
                        "description": "Show the Ferrum version"
                    }
                ]
            })
        );
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

fn write_fake_mcp_server(root: &Path) -> (PathBuf, PathBuf) {
    let script = root.join("fake-mcp.py");
    let pid_file = root.join("fake-mcp.pid");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(str(os.getpid()))

def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"client-test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"echo test","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}})
    elif method == "tools/call":
        result = {
            "text": message["params"]["arguments"].get("text"),
            "explicitValuePresent": os.environ.get("ACP_EXPLICIT") == "allowed",
            "providerCredentialPresent": "OPENAI_API_KEY" in os.environ,
        }
        send({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":json.dumps(result, separators=(",", ":"))}]}})
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script, permissions).unwrap();
    (script, pid_file)
}

fn wait_for_process_exit(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

fn run_mcp_echo_prompt(acp: &mut AcpProcess, id: u64, session_id: &str) -> (String, String) {
    acp.send(json!({
        "jsonrpc": "2.0", "id": id, "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "call dynamic MCP"}]
        }
    }));
    let mut tool_output = String::new();
    let mut assistant_text = String::new();
    loop {
        let message = acp.recv();
        if message["id"] == id {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        let update = &message["params"]["update"];
        match update["sessionUpdate"].as_str() {
            Some("tool_call_update") => {
                if let Some(text) = update["content"][0]["content"]["text"].as_str() {
                    tool_output.push_str(text);
                }
            }
            Some("agent_message_chunk") => {
                assistant_text.push_str(update["content"]["text"].as_str().unwrap());
            }
            _ => {}
        }
    }
    (tool_output, assistant_text)
}

#[test]
fn acp_stdio_runs_client_stdio_mcp_with_isolated_environment_and_cleanup() {
    let cwd = tempfile::tempdir().unwrap();
    let (script, pid_file) = write_fake_mcp_server(cwd.path());
    let mut acp = AcpProcess::spawn(cwd.path(), Some("mcp_echo"));
    acp.initialize();
    let mcp_server = || {
        json!({
            "name": "client",
            "command": script,
            "args": [pid_file],
            "env": [{"name": "ACP_EXPLICIT", "value": "allowed"}]
        })
    };
    acp.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [mcp_server()]}
    }));
    let created = acp.recv();
    assert_eq!(created["id"], 2, "session setup failed: {created}");
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_string();
    acp.assert_available_commands_update(&session_id);
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();

    let (tool_output, assistant_text) = run_mcp_echo_prompt(&mut acp, 3, &session_id);
    assert!(tool_output.contains("\"explicitValuePresent\":true"));
    assert!(tool_output.contains("\"providerCredentialPresent\":false"));
    assert!(assistant_text.contains("MCP result:"));

    acp.send(json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    assert!(
        wait_for_process_exit(pid),
        "MCP child survived session close"
    );

    acp.send(json!({
        "jsonrpc": "2.0", "id": 5, "method": "session/resume",
        "params": {
            "sessionId": session_id,
            "cwd": cwd.path(),
            "mcpServers": [mcp_server()]
        }
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    acp.assert_available_commands_update(&session_id);
    let resumed_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let (resumed_output, _) = run_mcp_echo_prompt(&mut acp, 6, &session_id);
    assert!(resumed_output.contains("\"providerCredentialPresent\":false"));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    assert!(
        wait_for_process_exit(resumed_pid),
        "resumed MCP child survived session close"
    );

    acp.send(json!({
        "jsonrpc": "2.0", "id": 8, "method": "session/load",
        "params": {
            "sessionId": session_id,
            "cwd": cwd.path(),
            "mcpServers": [mcp_server()]
        }
    }));
    loop {
        let message = acp.recv();
        if message["id"] == 8 {
            assert_eq!(message["result"], json!({}));
            break;
        }
        assert_eq!(message["method"], "session/update");
    }
    acp.assert_available_commands_update(&session_id);
    let loaded_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let (loaded_output, _) = run_mcp_echo_prompt(&mut acp, 9, &session_id);
    assert!(loaded_output.contains("\"explicitValuePresent\":true"));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 10, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    assert!(
        wait_for_process_exit(loaded_pid),
        "loaded MCP child survived session close"
    );
    acp.finish();
}

#[test]
fn acp_stdio_disconnect_cleans_up_client_mcp_processes() {
    let cwd = tempfile::tempdir().unwrap();
    let (script, pid_file) = write_fake_mcp_server(cwd.path());
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.initialize();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [{
            "name": "client", "command": script, "args": [pid_file], "env": []
        }]}
    }));
    assert!(acp.recv()["result"]["sessionId"].is_string());
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    acp.finish();
    assert!(
        wait_for_process_exit(pid),
        "MCP child survived ACP disconnect"
    );
}

#[test]
fn acp_stdio_rejects_invalid_or_failed_client_mcp_setup_without_secret_echo() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.initialize();

    acp.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [{
            "name": "relative", "command": "python3", "args": [], "env": []
        }]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);

    let secret = "dynamic-environment-secret-must-not-be-echoed";
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [{
            "name": "missing", "command": "/definitely/missing/ferrum-mcp", "args": [],
            "env": [{"name": "SECRET_VALUE", "value": secret}]
        }]}
    }));
    let failed = acp.recv();
    assert_eq!(failed["error"]["code"], -32603);
    assert!(!failed.to_string().contains(secret));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/list", "params": {}
    }));
    assert_eq!(acp.recv()["result"]["sessions"], json!([]));

    acp.send(json!({
        "jsonrpc": "2.0", "id": 5, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [
            {"name": "same", "command": "/bin/true", "args": [], "env": []},
            {"name": "same", "command": "/bin/true", "args": [], "env": []}
        ]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);

    acp.send(json!({
        "jsonrpc": "2.0", "id": 6, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [{
            "name": "x".repeat(257), "command": "/bin/true", "args": [], "env": []
        }]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);

    acp.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "session/new",
        "params": {"cwd": cwd.path(), "mcpServers": [{
            "type": "http", "name": "remote", "url": "https://example.invalid", "headers": []
        }]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.finish();
}

#[test]
fn acp_stdio_executes_only_advertised_headless_commands() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), None);
    acp.initialize();
    let session_id = acp.new_session(cwd.path());

    acp.send(json!({
        "jsonrpc": "2.0", "id": 30, "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "/version"}]
        }
    }));
    let mut output = String::new();
    loop {
        let message = acp.recv();
        if message["id"] == 30 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        assert_eq!(message["method"], "session/update");
        assert_eq!(
            message["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        output.push_str(
            message["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap(),
        );
    }
    assert_eq!(output, format!("ferrum {}", env!("CARGO_PKG_VERSION")));

    for (id, command, expected) in [
        (33, "/session", "path: "),
        (34, "/compact", "compaction skipped:"),
    ] {
        acp.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": command}]
            }
        }));
        let mut command_output = String::new();
        loop {
            let message = acp.recv();
            if message["id"] == id {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
            assert_eq!(
                message["params"]["update"]["sessionUpdate"],
                "agent_message_chunk"
            );
            command_output.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap(),
            );
        }
        assert!(command_output.contains(expected), "{command_output}");
    }

    for (id, command) in [(31, "/quit"), (32, "/session unexpected")] {
        acp.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": command}]
            }
        }));
        let response = acp.recv();
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32602);
    }
    acp.finish();
}

#[test]
fn acp_stdio_permission_approval_and_rejection_only_restrict_execution() {
    for (allow, request_id) in [(true, 40), (false, 41)] {
        let cwd = tempfile::tempdir().unwrap();
        let mut acp = AcpProcess::spawn_asking(cwd.path(), Some("permission_write"));
        acp.initialize();
        let session_id = acp.new_session(cwd.path());
        acp.send(json!({
            "jsonrpc": "2.0", "id": request_id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "write"}]
            }
        }));

        let permission_id = loop {
            let message = acp.recv();
            if message["method"] == "session/request_permission" {
                assert_eq!(message["params"]["sessionId"], session_id);
                assert_eq!(
                    message["params"]["options"],
                    json!([
                        {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"}
                    ])
                );
                break message["id"].clone();
            }
            assert_eq!(message["method"], "session/update");
        };
        acp.send(json!({
            "jsonrpc": "2.0",
            "id": permission_id,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": if allow {"allow_once"} else {"reject_once"}
                }
            }
        }));
        loop {
            let message = acp.recv();
            if message["id"] == request_id {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
            assert_eq!(message["method"], "session/update");
        }
        assert_eq!(cwd.path().join("permission.txt").exists(), allow);
        acp.finish();
    }
}

#[test]
fn acp_stdio_permission_request_cancels_with_prompt() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn_asking(cwd.path(), Some("permission_write"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 42, "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "cancel permission"}]
        }
    }));
    let permission_id = loop {
        let message = acp.recv();
        if message["method"] == "session/request_permission" {
            break message["id"].clone();
        }
    };
    acp.send(json!({
        "jsonrpc": "2.0", "method": "session/cancel",
        "params": {"sessionId": session_id}
    }));
    acp.send(json!({
        "jsonrpc": "2.0", "id": permission_id,
        "result": {"outcome": {"outcome": "cancelled"}}
    }));
    loop {
        let message = acp.recv();
        if message["id"] == 42 {
            assert_eq!(message["result"]["stopReason"], "cancelled");
            break;
        }
        assert_eq!(message["method"], "session/update");
    }
    assert!(!cwd.path().join("permission.txt").exists());
    acp.finish();
}

#[test]
fn acp_stdio_handles_concurrent_session_permission_requests() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn_asking(cwd.path(), Some("permission_write"));
    acp.initialize();
    let first_session = acp.new_session(cwd.path());
    let second_session = acp.new_session(cwd.path());
    for (id, session_id) in [(60, &first_session), (61, &second_session)] {
        acp.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "concurrent permission"}]
            }
        }));
    }
    let mut permission_ids = Vec::new();
    let mut completed = Vec::new();
    while completed.len() < 2 {
        let message = acp.recv();
        if message["method"] == "session/request_permission" {
            permission_ids.push(message["id"].clone());
            acp.send(json!({
                "jsonrpc": "2.0", "id": message["id"],
                "result": {
                    "outcome": {"outcome": "selected", "optionId": "reject_once"}
                }
            }));
        } else if let Some(id) = message["id"].as_i64()
            && matches!(id, 60 | 61)
        {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            completed.push(id);
        } else {
            assert_eq!(message["method"], "session/update");
        }
    }
    permission_ids.sort_by_key(Value::to_string);
    permission_ids.dedup();
    completed.sort_unstable();
    assert_eq!(permission_ids.len(), 2);
    assert_eq!(completed, vec![60, 61]);
    assert!(!cwd.path().join("permission.txt").exists());
    acp.finish();
}

#[test]
fn acp_stdio_permission_disconnect_cancels_pending_request() {
    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn_asking(cwd.path(), Some("permission_write"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 62, "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "disconnect permission"}]
        }
    }));
    loop {
        if acp.recv()["method"] == "session/request_permission" {
            break;
        }
    }
    acp.close_stdout();
    let _ = acp.finish_after_disconnect();
    assert!(!cwd.path().join("permission.txt").exists());
}

#[test]
fn acp_stdio_permission_policy_denials_never_become_client_choices() {
    for (script, safety, request_id) in [
        ("permission_outside", "medium", 50),
        ("permission_protected", "low", 51),
        ("permission_write", "high", 52),
        ("permission_bash_denied", "medium", 53),
    ] {
        let cwd = tempfile::tempdir().unwrap();
        let mut acp = AcpProcess::spawn_asking_with_safety(cwd.path(), Some(script), safety);
        acp.initialize();
        let session_id = acp.new_session(cwd.path());
        acp.send(json!({
            "jsonrpc": "2.0", "id": request_id, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "denied"}]
            }
        }));
        loop {
            let message = acp.recv();
            assert_ne!(message["method"], "session/request_permission", "{message}");
            if message["id"] == request_id {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
            assert_eq!(message["method"], "session/update");
        }
        assert!(!cwd.path().join("permission.txt").exists());
        acp.finish();
    }

    let cwd = tempfile::tempdir().unwrap();
    let mut acp = AcpProcess::spawn_asking_no_tools(cwd.path(), Some("permission_write"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 54, "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "disabled tool"}]
        }
    }));
    loop {
        let message = acp.recv();
        assert_ne!(message["method"], "session/request_permission", "{message}");
        if message["id"] == 54 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
    }
    assert!(!cwd.path().join("permission.txt").exists());
    acp.finish();
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
fn acp_stdio_persists_lists_loads_resumes_closes_and_deletes_sessions() {
    let cwd = tempfile::tempdir().unwrap();
    let other_cwd = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    let session_id = {
        let mut acp = AcpProcess::spawn_in(cwd.path(), None, storage.path());
        acp.initialize();
        let session_id = acp.new_session(cwd.path());
        acp.send(json!({
            "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "persisted"}]
            }
        }));
        loop {
            let message = acp.recv();
            if message["id"] == 3 {
                assert_eq!(message["result"]["stopReason"], "end_turn");
                break;
            }
        }
        acp.send(json!({
            "jsonrpc": "2.0", "id": 4, "method": "session/close",
            "params": {"sessionId": session_id}
        }));
        assert_eq!(acp.recv()["result"], json!({}));
        acp.finish();
        session_id
    };

    let malformed_id = "malformed-session";
    let sessions_dir = storage.path().join("data/sessions");
    let persisted_path = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let persisted = std::fs::read_to_string(persisted_path).unwrap();
    let header: Value = serde_json::from_str(persisted.lines().next().unwrap()).unwrap();
    for ambiguous_id in ["ambiguous-one", "ambiguous-two"] {
        let mut ambiguous_header = header.clone();
        ambiguous_header["id"] = Value::String(ambiguous_id.to_string());
        std::fs::write(
            sessions_dir.join(format!("{ambiguous_id}.jsonl")),
            format!("{}\n", serde_json::to_string(&ambiguous_header).unwrap()),
        )
        .unwrap();
    }
    let mut malformed_header = header;
    malformed_header["id"] = Value::String(malformed_id.to_string());
    std::fs::write(
        sessions_dir.join("malformed-session.jsonl"),
        format!(
            "{}\n{{not json\n",
            serde_json::to_string(&malformed_header).unwrap()
        ),
    )
    .unwrap();

    let mut acp = AcpProcess::spawn_in(cwd.path(), None, storage.path());
    acp.initialize();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 5, "method": "session/resume",
        "params": {"sessionId": "ambiguous", "cwd": cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32002);
    std::fs::remove_file(sessions_dir.join("ambiguous-one.jsonl")).unwrap();
    std::fs::remove_file(sessions_dir.join("ambiguous-two.jsonl")).unwrap();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 9, "method": "session/load",
        "params": {"sessionId": malformed_id, "cwd": cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32603);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 8, "method": "session/prompt",
        "params": {"sessionId": malformed_id, "prompt": [{"type": "text", "text": "x"}]}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32002);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 6, "method": "session/resume",
        "params": {"sessionId": malformed_id, "cwd": cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32603);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "session/delete",
        "params": {"sessionId": malformed_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 10, "method": "session/list", "params": {}
    }));
    let listed = acp.recv();
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["sessionId"], session_id);
    assert_eq!(sessions[0]["cwd"], cwd.path().to_string_lossy().as_ref());
    assert_eq!(sessions[0]["title"], "persisted");
    assert!(listed["result"].get("nextCursor").is_none());

    acp.send(json!({
        "jsonrpc": "2.0", "id": 11, "method": "session/load",
        "params": {"sessionId": session_id, "cwd": cwd.path(), "mcpServers": []}
    }));
    let mut replayed_user = String::new();
    let mut replayed_agent = String::new();
    loop {
        let message = acp.recv();
        if message["id"] == 11 {
            assert_eq!(message["result"], json!({}));
            break;
        }
        assert_eq!(message["method"], "session/update");
        assert_eq!(message["params"]["sessionId"], session_id);
        match message["params"]["update"]["sessionUpdate"].as_str() {
            Some("user_message_chunk") => replayed_user.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap(),
            ),
            Some("agent_message_chunk") => replayed_agent.push_str(
                message["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap(),
            ),
            other => panic!("unexpected replay update: {other:?}"),
        }
    }
    assert_eq!(replayed_user, "persisted");
    assert_eq!(replayed_agent, "fake provider response: persisted\n");
    acp.assert_available_commands_update(&session_id);

    acp.send(json!({
        "jsonrpc": "2.0", "id": 12, "method": "session/delete",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 13, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));

    acp.send(json!({
        "jsonrpc": "2.0", "id": 14, "method": "session/resume",
        "params": {"sessionId": session_id, "cwd": other_cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 15, "method": "session/resume",
        "params": {"sessionId": session_id, "cwd": cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    acp.assert_available_commands_update(&session_id);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 16, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));

    acp.send(json!({
        "jsonrpc": "2.0", "id": 17, "method": "session/delete",
        "params": {"sessionId": "../sessions"}
    }));
    assert_eq!(acp.recv()["error"]["code"], -32602);
    acp.send(json!({
        "jsonrpc": "2.0", "id": 18, "method": "session/delete",
        "params": {"sessionId": session_id}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    acp.send(json!({
        "jsonrpc": "2.0", "id": 19, "method": "session/list", "params": {}
    }));
    assert_eq!(acp.recv()["result"]["sessions"], json!([]));
    acp.finish();
}

#[test]
fn acp_stdio_can_resume_a_print_mode_session() {
    let cwd = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let config = storage.path().join("config");
    let data = storage.path().join("data");
    std::fs::create_dir(&config).unwrap();
    std::fs::create_dir(&data).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ferrum"))
        .arg("-p")
        .arg("from print mode")
        .current_dir(cwd.path())
        .env("FERRUM_CONFIG_DIR", &config)
        .env("FERRUM_DATA_DIR", &data)
        .env("FERRUM_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "print mode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut acp = AcpProcess::spawn_in(cwd.path(), None, storage.path());
    acp.initialize();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 20, "method": "session/list",
        "params": {"cwd": cwd.path()}
    }));
    let listed = acp.recv();
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["title"], "from print mode");
    let session_id = sessions[0]["sessionId"].as_str().unwrap();
    acp.send(json!({
        "jsonrpc": "2.0", "id": 21, "method": "session/resume",
        "params": {"sessionId": session_id, "cwd": cwd.path(), "mcpServers": []}
    }));
    assert_eq!(acp.recv()["result"], json!({}));
    acp.assert_available_commands_update(session_id);
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
        "jsonrpc": "2.0", "id": 5, "method": "session/close",
        "params": {"sessionId": session_id}
    }));
    let busy = acp.recv();
    assert_eq!(busy["id"], 5);
    assert_eq!(busy["error"]["code"], -32602);
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
fn acp_stdio_applies_restrictive_project_config_per_session_cwd() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cwd.path().join(".ferrum")).unwrap();
    std::fs::write(
        cwd.path().join(".ferrum/config.toml"),
        "safety = \"high\"\n[tools]\nallow = [\"write\"]\nwritable_roots = [\".\"]\n",
    )
    .unwrap();
    let mut acp = AcpProcess::spawn(cwd.path(), Some("permission_write"));
    acp.initialize();
    let session_id = acp.new_session(cwd.path());
    acp.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]}
    }));
    let mut saw_policy_failure = false;
    loop {
        let message = acp.recv();
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
        if message["params"]["update"]["sessionUpdate"] == "tool_call_update"
            && message["params"]["update"]["status"] == "failed"
        {
            saw_policy_failure = message["params"]["update"]["content"]
                .to_string()
                .contains("not authorized at high safety");
        }
    }
    assert!(saw_policy_failure);
    assert!(!cwd.path().join("permission.txt").exists());
    acp.finish();
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
