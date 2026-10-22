use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn run_print(perf: bool) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let data = root.path().join("data");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ferrum"));
    if perf {
        command.arg("--perf");
    }
    command
        .args(["-p", "hello"])
        .current_dir(root.path())
        .env("FERRUM_CONFIG_DIR", config)
        .env("FERRUM_DATA_DIR", data)
        .env("FERRUM_OFFLINE", "1")
        .output()
        .unwrap()
}

#[test]
fn print_perf_is_opt_in_and_keeps_stdout_pipe_safe() {
    let normal = run_print(false);
    assert!(normal.status.success());
    assert_eq!(
        String::from_utf8(normal.stdout).unwrap(),
        "fake provider response: hello\n"
    );
    assert!(!String::from_utf8(normal.stderr).unwrap().contains("perf:"));

    let measured = run_print(true);
    assert!(measured.status.success());
    assert_eq!(
        String::from_utf8(measured.stdout).unwrap(),
        "fake provider response: hello\n"
    );
    let stderr = String::from_utf8(measured.stderr).unwrap();
    assert!(stderr.starts_with("\nperf:"));
    let perf = stderr
        .lines()
        .find(|line| line.starts_with("perf:"))
        .expect("missing performance summary");
    assert!(perf.contains("1 request"));
    assert!(perf.contains("TTFT n/a"));
    assert!(perf.contains("n/a t/s"));
    assert!(perf.contains("tot ~"));
    assert!(!perf.contains("final"));
    assert!(!perf.contains("Speed"));
    assert!(!perf.contains("output"));
    assert!(perf.contains("turn "));
}

#[test]
fn print_perf_aggregates_streamed_tool_and_reasoning_requests() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let data = root.path().join("data");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        root.path().join("fixture.txt"),
        "tool result must not count as model output\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for attempt in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing provider request");
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
            thread::sleep(Duration::from_millis(10));
            let delta = if attempt == 0 {
                json!({"tool_calls":[{"index":0,"id":"perf-read","type":"function","function":{"name":"read","arguments":"{\"path\":\"fixture.txt\"}"}}]})
            } else {
                json!({"reasoning_content":"displayable provider reasoning"})
            };
            write_sse(&mut socket, json!({"choices":[{"index":0,"delta":delta}]}));
            thread::sleep(Duration::from_millis(30));
            if attempt == 1 {
                write_sse(
                    &mut socket,
                    json!({"choices":[{"index":0,"delta":{"content":"aggregate answer\n"}}]}),
                );
            }
            let tokens = if attempt == 0 { 100 } else { 1_474 };
            write_sse(
                &mut socket,
                json!({"choices":[],"usage":{
                    "prompt_tokens":20,"completion_tokens":tokens,"total_tokens":20 + tokens,
                    "completion_tokens_details":{"reasoning_tokens":if attempt == 0 { 0 } else { 1_400 }}
                }}),
            );
            let done = "data: [DONE]\n\n";
            write!(socket, "{:x}\r\n{done}\r\n0\r\n\r\n", done.len()).unwrap();
            socket.flush().unwrap();
        }
        requests
    });
    std::fs::write(config.join("config.toml"), format!(
        "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\ntype = \"openai-compatible\"\nbase_url = \"http://{address}/v1\"\n"
    )).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ferrum"))
        .args(["--perf", "-p", "check turn-wide performance"])
        .current_dir(root.path())
        .env("FERRUM_CONFIG_DIR", config)
        .env("FERRUM_DATA_DIR", &data)
        .env_remove("FERRUM_OFFLINE")
        .env_remove("FERRUM_FAKE_SCRIPT")
        .output()
        .unwrap();
    let requests = server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("aggregate answer\n")
    );
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool")
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let perf = stderr
        .lines()
        .find(|line| line.starts_with("perf:"))
        .unwrap();
    let fields = perf.split(" | ").collect::<Vec<_>>();
    assert_eq!(fields.len(), 6, "{perf}");
    assert_eq!(fields[0], "perf: 2 requests");
    assert_eq!(fields[3], "tot 1,574 tok");
    assert!(fields[4].starts_with("ctx "), "{perf}");
    assert!(fields[4].contains('/'), "{perf}");
    assert!(fields[4].ends_with('%'), "{perf}");
    assert!(fields[5].starts_with("turn "), "{perf}");
    assert!(!perf.contains("final"));
    assert!(!perf.contains("Speed"));
    assert!(!perf.contains('~'));
    let ttft = fields[1].strip_prefix("TTFT ").unwrap();
    let ttft = ttft
        .strip_suffix("ms")
        .or_else(|| ttft.strip_suffix('s'))
        .unwrap();
    let rates = fields[2].strip_suffix(" t/s").unwrap();
    for triplet in [ttft, rates] {
        let values = triplet
            .split('/')
            .map(|value| value.parse::<f64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 3, "{perf}");
        assert!(values[0] <= values[1] && values[1] <= values[2], "{perf}");
        assert!(values.iter().all(|value| value.is_finite()), "{perf}");
    }
    let usage = std::fs::read_to_string(data.join("usage.jsonl")).unwrap();
    let total = usage
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["output_tokens"]
                .as_u64()
                .unwrap()
        })
        .sum::<u64>();
    assert_eq!(total, 1_574);
}

fn write_sse(socket: &mut impl Write, event: Value) {
    let data = format!("data: {event}\n\n");
    write!(socket, "{:x}\r\n{data}\r\n", data.len()).unwrap();
    socket.flush().unwrap();
}
