use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("config")).unwrap();
        fs::write(
            root.path().join("config/config.toml"),
            r#"
provider = "fallback"
model = "fallback-model"
max_context_tokens = 256000

[providers.fallback]
type = "fake"
default_model = "fallback-model"
[providers.saved]
type = "fake"
default_model = "saved-default"
[providers.other]
type = "fake"
default_model = "other-default"

[models.saved-model]
actual_model = "saved-upstream"
max_context_tokens = 100000
[models.saved-default]
actual_model = "saved-default-upstream"
max_context_tokens = 110000
[models.other-default]
actual_model = "other-default-upstream"
max_context_tokens = 90000
[models.requested]
actual_model = "requested-upstream"
max_context_tokens = 70000
[models.routed]
provider = "other"
actual_model = "routed-upstream"
max_context_tokens = 80000
[models.tiny]
actual_model = "tiny-upstream"
max_context_tokens = 6000
"#,
        )
        .unwrap();
        Self { root }
    }

    fn run(&self, args: &[&str], prompt: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ferrum"))
            .current_dir(self.root.path())
            .env("FERRUM_CONFIG_DIR", self.root.path().join("config"))
            .env("FERRUM_DATA_DIR", self.root.path().join("data"))
            .env_remove("FERRUM_OFFLINE")
            .env_remove("OPENAI_API_KEY")
            .env("FERRUM_FAKE_SCRIPT", "inspect_images")
            .args(["--no-mcp", "--no-tools", "--perf"])
            .args(args)
            .args(["-p", prompt])
            .output()
            .unwrap()
    }

    fn seed(&self) {
        let output = self.run(
            &[
                "--session",
                "resume-test",
                "--provider",
                "saved",
                "--model",
                "saved-model",
            ],
            "seed",
        );
        assert_success(&output);
        self.assert_selection("saved", "saved-upstream", "100k", &output);
    }

    fn session_path(&self) -> PathBuf {
        self.root.path().join("data/sessions/resume-test.jsonl")
    }

    fn usage(&self) -> Vec<Value> {
        fs::read_to_string(self.root.path().join("data/usage.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn assert_selection(&self, provider: &str, model: &str, context: &str, output: &Output) {
        let usage = self.usage();
        let last = usage.last().unwrap();
        assert_eq!(last["provider"], provider);
        assert_eq!(last["model"], model);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("/{context} ")),
            "wrong context budget: {stderr}"
        );
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn print_resume_cli_selection_overrides_saved_provider_and_model() {
    let cases: &[(&[&str], &str, &str, &str)] = &[
        (&[], "saved", "saved-upstream", "100k"),
        (
            &["--model", "requested"],
            "saved",
            "requested-upstream",
            "70k",
        ),
        (&["--model", "raw-model"], "saved", "raw-model", "256k"),
        (&["--model", "routed"], "other", "routed-upstream", "80k"),
        (
            &["--provider", "other"],
            "other",
            "other-default-upstream",
            "90k",
        ),
        (
            &["--provider", "other", "--model", "requested"],
            "other",
            "requested-upstream",
            "70k",
        ),
    ];
    for flag in ["--session", "--resume"] {
        for (overrides, provider, model, context) in cases {
            let fixture = Fixture::new();
            fixture.seed();
            let path = fixture.session_path();
            let reference = if flag == "--session" {
                "resume-test"
            } else {
                path.to_str().unwrap()
            };
            let mut args = vec![flag, reference];
            args.extend_from_slice(overrides);
            let output = fixture.run(&args, "resumed");
            assert_success(&output);
            fixture.assert_selection(provider, model, context, &output);
            // Runtime overrides do not rewrite the saved provider/model preference.
            let restored = fixture.run(&[flag, reference], "restored");
            assert_success(&restored);
            fixture.assert_selection("saved", "saved-upstream", "100k", &restored);
        }
    }
}

#[test]
fn print_resume_model_override_enforces_its_smaller_context_budget() {
    for flag in ["--session", "--resume"] {
        let fixture = Fixture::new();
        fixture.seed();
        let path = fixture.session_path();
        let reference = path.to_str().unwrap();
        let before = fixture.usage().len();
        let output = fixture.run(
            &[flag, reference, "--model", "tiny"],
            &"document ".repeat(4000),
        );
        assert!(
            !output.status.success(),
            "oversized request reached the model instead of using its smaller budget"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("safe request budget"), "{stderr}");
        assert!(stderr.contains("/6000 tokens)"), "{stderr}");
        assert_eq!(
            fixture.usage().len(),
            before,
            "an ordinary model request was sent after budget failure"
        );
    }
}

#[test]
fn print_resume_explicit_selection_ignores_removed_saved_provider() {
    for overrides in [vec!["--provider", "other"], vec!["--model", "routed"]] {
        let fixture = Fixture::new();
        fixture.seed();
        let path = fixture.root.path().join("config/config.toml");
        let config = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            config.replace("[providers.saved]", "[providers.removed]"),
        )
        .unwrap();
        let mut args = vec!["--session", "resume-test"];
        args.extend_from_slice(&overrides);
        let output = fixture.run(&args, "use explicit selection");
        assert_success(&output);
        if overrides[0] == "--provider" {
            fixture.assert_selection("other", "other-default-upstream", "90k", &output);
        } else {
            fixture.assert_selection("other", "routed-upstream", "80k", &output);
        }
        let unqualified = fixture.run(&["--session", "resume-test"], "restore missing provider");
        assert!(!unqualified.status.success());
    }
}

#[test]
fn print_resume_http_request_uses_overrides_and_preserves_safety_constraints() {
    for (project_safety, expected_safety) in [(None, "medium"), (Some("high"), "high")] {
        let fixture = Fixture::new();
        let seeded = fixture.run(
            &[
                "--session",
                "resume-test",
                "--provider",
                "saved",
                "--model",
                "saved-model",
                "--thinking",
                "xhigh",
                "--safety",
                "low",
            ],
            "seed",
        );
        assert_success(&seeded);
        if let Some(safety) = project_safety {
            fs::create_dir(fixture.root.path().join(".ferrum")).unwrap();
            fs::write(
                fixture.root.path().join(".ferrum/config.toml"),
                format!("safety = \"{safety}\"\n"),
            )
            .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let config_path = fixture.root.path().join("config/config.toml");
        let config = fs::read_to_string(&config_path).unwrap();
        fs::write(&config_path, config.replace("[providers.other]\ntype = \"fake\"", &format!("[providers.other]\ntype = \"openai-compatible\"\nbase_url = \"http://{address}/v1\"\nstreaming = false\ntimeout_seconds = 2"))).unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "explicit provider never received a request"
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
            let request = serde_json::from_slice::<Value>(&body).unwrap();
            let response = json!({"choices":[{"message":{"role":"assistant","content":"override confirmed"},"finish_reason":"stop"}]}).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            socket.flush().unwrap();
            request
        });
        let output = fixture.run(
            &[
                "--session",
                "resume-test",
                "--provider",
                "other",
                "--model",
                "requested",
                "--thinking",
                "low",
                "--safety",
                "medium",
            ],
            "verify overrides",
        );
        let request = server.join().unwrap();
        assert_success(&output);
        fixture.assert_selection("other", "requested-upstream", "70k", &output);
        assert_eq!(request["model"], "requested-upstream");
        assert!(
            request
                .get("tools")
                .is_none_or(|tools| tools.as_array().unwrap().is_empty())
        );
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .map(|message| message["content"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for field in [
            "provider: other",
            "model: requested",
            "provider_model: requested-upstream",
            "max_context_tokens: 70000",
            "thinking: low",
            &format!("safety: {expected_safety}"),
        ] {
            assert!(system.contains(field), "missing runtime field {field}");
        }
    }
}
