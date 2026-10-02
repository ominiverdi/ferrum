use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

fn command(root: &Path, script: Option<&str>) -> Command {
    let config = root.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ferrum"));
    command
        .current_dir(root)
        .env("FERRUM_CONFIG_DIR", config)
        .env("FERRUM_DATA_DIR", root.join("data"))
        .env("FERRUM_OFFLINE", "1")
        .env_remove("FERRUM_FAKE_SCRIPT");
    if let Some(script) = script {
        command.env("FERRUM_FAKE_SCRIPT", script);
    }
    command
}

fn run_script(script: &str, perf: bool) -> (tempfile::TempDir, Output) {
    let root = tempfile::tempdir().unwrap();
    let mut command = command(root.path(), Some(script));
    if perf {
        command.arg("--perf");
    }
    let output = command
        .args(["-p", "check loop recovery"])
        .output()
        .unwrap();
    (root, output)
}

#[test]
fn print_stream_loop_recovers_and_counts_interrupted_usage() {
    for script in ["repeat_text_recover", "repeat_thinking"] {
        let (root, output) = run_script(script, true);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "recovered concise response\n"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("interrupted model response"), "{stderr}");
        assert!(stderr.contains("perf: 2 requests"), "{stderr}");
        let usage = std::fs::read_to_string(root.path().join("data/usage.jsonl")).unwrap();
        let records = usage
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["source"], "estimated");
        assert!(records[0]["output_tokens"].as_u64().unwrap() > 0);
    }
}

#[test]
fn buffered_print_loop_stops_after_bounded_recovery() {
    let (_root, output) = run_script("repeat_text", false);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("stopped without further retries"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("interrupted model response").count(), 2);
}

#[test]
fn print_sequence_loop_reaches_final_synthesis() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("loop.txt"), "first\nsecond\n").unwrap();
    let output = command(root.path(), Some("loop_sequence"))
        .args(["-p", "check alternating reads"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stdout.ends_with("final after tool sequence loop guard\n"),
        "{stdout}"
    );
    assert!(
        stderr.contains("2-call tool sequence repeated 2 times"),
        "{stderr}"
    );
    assert!(
        stderr.contains("2-call tool sequence repeated 3 times"),
        "{stderr}"
    );
}

#[test]
fn real_sse_loop_is_cancelled_and_not_replayed_in_recovery() {
    for stream_field in ["content", "reasoning_content"] {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            let mut sent = 0;
            for attempt in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing recovery request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length > 0 && length < 100_000);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice::<Value>(&body).unwrap());
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                if attempt == 0 {
                    let repeated = "The streamed model repeats this explanation rather than making progress with a new concrete result for the user's task. ";
                    for _ in 0..100 {
                        let data = format!(
                            "data: {}\n\n",
                            json!({"choices":[{"index":0,"delta":{stream_field:repeated}}]})
                        );
                        if write_chunk(&mut socket, &data).is_err() {
                            break;
                        }
                        sent += 1;
                        thread::sleep(Duration::from_millis(5));
                    }
                } else {
                    let data = format!(
                        "data: {}\n\n",
                        json!({"choices":[{"index":0,"delta":{"content":"clean SSE recovery\n"}}]})
                    );
                    write_chunk(&mut socket, &data).unwrap();
                    write_chunk(&mut socket, "data: [DONE]\n\n").unwrap();
                    socket.write_all(b"0\r\n\r\n").unwrap();
                }
            }
            (requests, sent)
        });
        let mut command = command(root.path(), None);
        std::fs::write(root.path().join("config/config.toml"), format!(
            "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\ntype = \"openai-compatible\"\nbase_url = \"http://{address}/v1\"\n"
        )).unwrap();
        command.env_remove("FERRUM_OFFLINE");
        let output = command
            .args(["--perf", "-p", "check SSE repetition"])
            .output()
            .unwrap();
        let (requests, sent) = server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "clean SSE recovery\n"
        );
        assert!(
            sent < 20,
            "repeating SSE stream was not cancelled early: {sent}"
        );
        assert_eq!(requests.len(), 2);
        let recovery = &requests[1]["messages"];
        assert!(
            recovery
                .to_string()
                .contains("Its partial output was discarded")
        );
        assert!(!recovery.to_string().contains("The streamed model repeats"));
    }
}

fn write_chunk(socket: &mut impl Write, data: &str) -> std::io::Result<()> {
    write!(socket, "{:x}\r\n{data}\r\n", data.len())?;
    socket.flush()
}
