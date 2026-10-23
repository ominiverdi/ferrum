use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn receive_request(listener: &TcpListener) -> (TcpStream, Value) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "missing image-read provider request"
                );
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
    (socket, serde_json::from_slice(&body).unwrap())
}

#[test]
fn print_read_image_sends_pixels_in_real_chat_requests_without_payload_output() {
    for streaming in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(3, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let payload = STANDARD.encode(&bytes);
        std::fs::write(root.path().join("image.png"), bytes).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let expected_payload = payload.clone();
        let server = thread::spawn(move || {
            for index in 0..2 {
                let (mut socket, request) = receive_request(&listener);
                assert_eq!(request["stream"], streaming);
                if index == 1 {
                    let messages = request["messages"].as_array().unwrap();
                    let tool_index = messages
                        .iter()
                        .position(|message| message["role"] == "tool")
                        .unwrap();
                    assert_eq!(messages[tool_index]["tool_call_id"], "image_read");
                    assert!(
                        messages[tool_index]["content"]
                            .as_str()
                            .unwrap()
                            .contains("image content attached")
                    );
                    assert_eq!(messages[tool_index + 1]["role"], "user");
                    let image = &messages[tool_index + 1]["content"][1];
                    assert_eq!(image["type"], "image_url");
                    assert_eq!(
                        image["image_url"]["url"],
                        format!("data:image/png;base64,{expected_payload}")
                    );
                }
                let message = if index == 0 {
                    json!({"role":"assistant", "tool_calls":[{"index":0,"id":"image_read", "type":"function", "function":{"name":"read", "arguments":"{\"path\":\"image.png\"}"}}]})
                } else {
                    json!({"role":"assistant", "content":"pixels received"})
                };
                let body = if streaming {
                    format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"index":0,"delta":message,"finish_reason":if index == 0 {"tool_calls"} else {"stop"}}]})
                    )
                } else {
                    json!({"choices":[{"message":message,"finish_reason":if index == 0 {"tool_calls"} else {"stop"}}]}).to_string()
                };
                let content_type = if streaming {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                socket.flush().unwrap();
            }
        });
        std::fs::write(config.join("config.toml"), format!(
            "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\ntype = \"openai-compatible\"\nbase_url = \"http://{address}/v1\"\ntimeout_seconds = 2\n"
        )).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ferrum"));
        command
            .current_dir(root.path())
            .env("FERRUM_CONFIG_DIR", config)
            .env("FERRUM_DATA_DIR", root.path().join("data"))
            .env_remove("FERRUM_OFFLINE")
            .env_remove("FERRUM_FAKE_SCRIPT")
            .env_remove("OPENAI_API_KEY");
        if streaming {
            command.arg("--perf");
        }
        let output = command
            .args(["-p", "inspect the generated screenshot"])
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.ends_with("pixels received\n"));
        assert!(!stdout.contains(&payload));
        assert!(!stderr.contains(&payload));
        assert!(!stdout.contains("data:image"));
        assert!(!stderr.contains("data:image"));
    }
}
