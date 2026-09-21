use std::process::Command;

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
    assert!(perf.contains("final ttft n/a"));
    assert!(perf.contains("final n/a"));
    assert!(perf.contains("output ~"));
    assert!(perf.contains("turn "));
}
