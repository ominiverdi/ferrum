use crate::config::SafetyLevel;
use std::path::{Path, PathBuf};

pub(super) fn assignment_changes_authority(name: &str) -> bool {
    matches!(
        name,
        "PATH"
            | "HOME"
            | "PWD"
            | "OLDPWD"
            | "IFS"
            | "BASH_ENV"
            | "ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "CDPATH"
            | "GLOBIGNORE"
            | "LD_PRELOAD"
            | "LD_LIBRARY_PATH"
            | "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_CONFIG"
            | "GIT_CONFIG_COUNT"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_SSH_COMMAND"
            | "CARGO_HOME"
            | "CARGO_TARGET_DIR"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "MAKEFLAGS"
            | "PYTHONPATH"
            | "PYTHONSTARTUP"
            | "NODE_OPTIONS"
            | "RUBYOPT"
            | "PERL5OPT"
    ) || name.starts_with("LD_")
        || name.starts_with("BASH_FUNC_")
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

pub(super) fn evaluate_command(
    command: &[String],
    safety: SafetyLevel,
    cwd: &Path,
    writable_roots: &[PathBuf],
) -> Option<String> {
    evaluate_command_inner(command, safety, cwd, writable_roots, 0)
}

fn evaluate_command_inner(
    command: &[String],
    safety: SafetyLevel,
    cwd: &Path,
    writable_roots: &[PathBuf],
    depth: usize,
) -> Option<String> {
    if depth >= 4 {
        return Some("shell wrapper nesting exceeds safety limit".to_string());
    }
    let command_start = command
        .iter()
        .position(|word| !is_environment_assignment(word))?;
    for assignment in &command[..command_start] {
        if !matches!(safety, SafetyLevel::Low)
            && environment_assignment_name(assignment).is_some_and(assignment_changes_authority)
        {
            return Some("environment assignment changes execution authority".to_string());
        }
    }
    if command_start > 0 {
        return evaluate_command_inner(
            &command[command_start..],
            safety,
            cwd,
            writable_roots,
            depth + 1,
        );
    }

    let first = command.first()?;
    if first.contains('$') || first.contains(['*', '?', '[', '{']) {
        if matches!(safety, SafetyLevel::Low) {
            return None;
        }
        return Some("dynamic executable position".to_string());
    }
    let base = command_name(first).to_ascii_lowercase();
    let args = &command[1..];
    let all = command
        .iter()
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if matches!(safety, SafetyLevel::Medium | SafetyLevel::High) && is_detacher_command(&base) {
        return Some("detached process launcher".to_string());
    }

    if matches!(safety, SafetyLevel::High) && !high_safety_command_allowed(&base, args) {
        return Some(format!(
            "command authority is not allowed at high safety: {base}"
        ));
    }

    if let Some(payload) = wrapper_payload(&base, args, safety) {
        let payload = match payload {
            Ok(payload) => payload,
            Err(_) if matches!(safety, SafetyLevel::Low) => return None,
            Err(reason) => return Some(reason.to_string()),
        };
        if payload.is_empty() {
            if matches!(base.as_str(), "command" | "builtin") {
                return None;
            }
            return Some("wrapper command has no inspectable payload".to_string());
        }
        if evaluate_command_inner(payload, safety, cwd, writable_roots, depth + 1).is_some() {
            return Some("wrapper contains denied command".to_string());
        }
    }

    if !matches!(safety, SafetyLevel::Low) && is_uninspectable_executor(&base) {
        return Some("indirect executor boundary is not authorized".to_string());
    }

    if !matches!(safety, SafetyLevel::Low) && base == "xargs" {
        return Some("indirect command execution through xargs".to_string());
    }

    if !matches!(safety, SafetyLevel::Low) && is_shell_control_word(&base) {
        return Some("shell compound control syntax".to_string());
    }

    if !matches!(safety, SafetyLevel::Low)
        && base == "busybox"
        && args.first().is_some_and(|arg| is_shell_interpreter(arg))
    {
        return Some("shell interpreter invocation".to_string());
    }

    if matches!(safety, SafetyLevel::Medium | SafetyLevel::High)
        && script_interpreter_execution(&base, args)
    {
        return Some("script interpreter invocation".to_string());
    }

    if matches!(safety, SafetyLevel::High) && generated_code_execution(&base, args) {
        return Some("generated code execution command".to_string());
    }

    if base.starts_with("mkfs") {
        return Some("filesystem formatting command".to_string());
    }

    if matches!(
        base.as_str(),
        "sudo" | "su" | "doas" | "pkexec" | "run0" | "runuser"
    ) {
        return Some("privilege escalation command".to_string());
    }

    if is_dynamic_shell_builtin(&base) && !matches!(safety, SafetyLevel::Low) {
        return Some("dynamic shell execution command".to_string());
    }

    if base == "rm" && dangerous_rm(args, safety) {
        return Some("destructive rm command".to_string());
    }

    if base == "dd" && dangerous_dd(args, safety) {
        return Some("dd output write command".to_string());
    }

    if base == "chmod" && dangerous_chmod(args, safety) {
        return Some("dangerous chmod command".to_string());
    }

    if base == "chown"
        && args
            .iter()
            .any(|arg| arg.to_ascii_lowercase().contains("root"))
    {
        return Some("dangerous chown command".to_string());
    }

    if !matches!(safety, SafetyLevel::Low) && is_shell_interpreter(&base) {
        return Some("shell interpreter invocation".to_string());
    }

    if matches!(safety, SafetyLevel::Medium | SafetyLevel::High)
        && is_inline_script_interpreter(&base, args)
    {
        return Some("inline script interpreter invocation".to_string());
    }

    if matches!(safety, SafetyLevel::High) && is_network_tool(&base, args) {
        return Some("network-capable command".to_string());
    }

    if let Some(reason) = mutation_policy_reason(&base, args, safety, cwd, writable_roots) {
        return Some(reason);
    }

    if !matches!(safety, SafetyLevel::Low)
        && base == "base64"
        && args.iter().any(|arg| arg == "-d" || arg == "--decode")
    {
        return Some("encoded command decoding".to_string());
    }

    if !matches!(safety, SafetyLevel::Low) && base == "xxd" && args.iter().any(|arg| arg == "-r") {
        return Some("hex decoding command".to_string());
    }

    if !matches!(safety, SafetyLevel::Low)
        && ((base == "echo" && args.iter().any(|arg| arg == "-e")) || base == "printf")
        && all.iter().any(|arg| contains_escape_sequence(arg))
    {
        return Some("escape-sequence command construction".to_string());
    }

    if !matches!(safety, SafetyLevel::Low)
        && base == "find"
        && args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"
            )
        })
    {
        return Some("find command with execution or deletion action".to_string());
    }

    if base == "tar" && dangerous_tar(args, safety) {
        return Some("archive extraction to sensitive path".to_string());
    }

    if base == "install" && dangerous_install(args, safety) {
        return Some("privileged install command".to_string());
    }

    if base == "sed" && dangerous_sed(args, safety) {
        return Some("in-place edit of sensitive file".to_string());
    }

    if matches!(base.as_str(), "cp" | "mv")
        && args.iter().any(|arg| {
            let arg = arg.to_ascii_lowercase();
            is_tier_independent_protected_path(&arg)
                || !matches!(safety, SafetyLevel::Low) && is_sensitive_path(&arg)
        })
    {
        return Some("file operation targeting sensitive path".to_string());
    }

    if !matches!(safety, SafetyLevel::Low)
        && base == "history"
        && args.iter().any(|arg| arg == "-c")
    {
        return Some("history clearing command".to_string());
    }

    if !matches!(safety, SafetyLevel::Low)
        && base == "unset"
        && args.iter().any(|arg| arg.contains("HIST"))
    {
        return Some("history environment manipulation".to_string());
    }

    None
}

fn command_name(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

fn is_environment_assignment(word: &str) -> bool {
    environment_assignment_name(word).is_some()
}

fn environment_assignment_name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    let identifier = match name.split_once('[') {
        Some((identifier, subscript)) if subscript.ends_with(']') => identifier,
        Some(_) => return None,
        None => name,
    };
    let mut chars = identifier.chars();
    if chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        Some(identifier)
    } else {
        None
    }
}

fn wrapper_payload<'a>(
    command: &str,
    args: &'a [String],
    safety: SafetyLevel,
) -> Option<Result<&'a [String], &'static str>> {
    match command {
        "command" => {
            if args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "-v" | "-V"))
            {
                return None;
            }
            let mut index = 0;
            if args.first().is_some_and(|arg| arg == "-p") {
                index += 1;
            }
            while args.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
            if args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                return Some(Err("command options cannot be inspected safely"));
            }
            Some(Ok(&args[index..]))
        }
        "builtin" | "nohup" | "setsid" => {
            let mut index = 0;
            while args.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
            if args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                return Some(Err("wrapper options cannot be inspected safely"));
            }
            Some(Ok(&args[index..]))
        }
        "busybox" => {
            if args.first().is_some_and(|arg| arg.starts_with('-')) {
                return Some(Err("busybox options cannot be inspected safely"));
            }
            Some(Ok(args))
        }
        "stdbuf" => {
            let mut index = 0;
            while let Some(arg) = args.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                }
                if matches!(arg.as_str(), "-i" | "-o" | "-e") {
                    if args.get(index + 1).is_none() {
                        return Some(Err("stdbuf option is missing its mode"));
                    }
                    index += 2;
                    continue;
                }
                if arg.starts_with("--input=")
                    || arg.starts_with("--output=")
                    || arg.starts_with("--error=")
                    || (arg.starts_with("-i") || arg.starts_with("-o") || arg.starts_with("-e"))
                        && arg.len() > 2
                {
                    index += 1;
                    continue;
                }
                if arg.starts_with('-') {
                    return Some(Err("stdbuf options cannot be inspected safely"));
                }
                break;
            }
            Some(Ok(&args[index..]))
        }
        "env" => {
            let mut index = 0;
            while let Some(arg) = args.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                }
                if is_environment_assignment(arg) {
                    if !matches!(safety, SafetyLevel::Low)
                        && environment_assignment_name(arg)
                            .is_some_and(assignment_changes_authority)
                    {
                        return Some(Err("env assignment changes execution authority"));
                    }
                    index += 1;
                    continue;
                }
                if arg.starts_with('-') {
                    return Some(Err("env options cannot be inspected safely"));
                }
                break;
            }
            Some(Ok(&args[index..]))
        }
        "nice" => {
            let mut index = 0;
            if args
                .get(index)
                .is_some_and(|arg| matches!(arg.as_str(), "-n" | "--adjustment"))
            {
                if args.get(index + 1).is_none() {
                    return Some(Err("nice adjustment is missing"));
                }
                index += 2;
            } else if args
                .get(index)
                .is_some_and(|arg| arg.starts_with("--adjustment=") || numeric_nice_option(arg))
            {
                index += 1;
            } else if args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                return Some(Err("nice options cannot be inspected safely"));
            }
            Some(Ok(&args[index..]))
        }
        "timeout" => {
            let mut index = 0;
            if args.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            } else if args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                return Some(Err("timeout options cannot be inspected safely"));
            }
            let duration = args.get(index)?;
            if !valid_timeout_duration(duration) {
                return Some(Err("timeout duration cannot be inspected safely"));
            }
            Some(Ok(args.get(index + 1..).unwrap_or_default()))
        }
        _ => None,
    }
}

fn numeric_nice_option(value: &str) -> bool {
    value.strip_prefix('-').is_some_and(|value| {
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    })
}

fn is_uninspectable_executor(command: &str) -> bool {
    matches!(
        command,
        "parallel"
            | "ionice"
            | "chrt"
            | "taskset"
            | "numactl"
            | "prlimit"
            | "strace"
            | "ltrace"
            | "watch"
            | "entr"
            | "daemonize"
            | "disown"
            | "systemd-run"
            | "at"
            | "batch"
            | "chroot"
            | "unshare"
            | "nsenter"
            | "bwrap"
            | "flatpak-spawn"
            | "docker"
            | "podman"
            | "kubectl"
            | "sshpass"
    )
}

fn valid_timeout_duration(value: &str) -> bool {
    let value = value.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(value);
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
        && value.chars().filter(|character| *character == '.').count() <= 1
}

fn high_safety_command_allowed(command: &str, args: &[String]) -> bool {
    match command {
        "pwd" | "whoami" | "id" | "uname" | "date" | "printf" | "echo" | "true" | "false"
        | "test" | "[" | "ls" | "stat" | "cat" | "head" | "tail" | "wc" | "cut" | "sort"
        | "diff" | "cmp" | "grep" | "rg" | "find" | "readlink" | "realpath" => true,
        "git" => args.first().is_some_and(|arg| {
            matches!(
                arg.as_str(),
                "status" | "diff" | "log" | "show" | "rev-parse"
            )
        }),
        "command" | "builtin" | "env" | "nice" | "timeout" | "stdbuf" => true,
        _ => false,
    }
}

fn script_interpreter_execution(command: &str, args: &[String]) -> bool {
    if !matches!(
        command,
        "python" | "python2" | "python3" | "ruby" | "perl" | "php" | "node" | "nodejs"
    ) || args.is_empty()
    {
        return false;
    }
    if matches!(command, "python" | "python2" | "python3")
        && args
            .windows(2)
            .any(|pair| pair[0] == "-m" && matches!(pair[1].as_str(), "pytest" | "unittest"))
    {
        return false;
    }
    true
}

fn mutation_policy_reason(
    command: &str,
    args: &[String],
    safety: SafetyLevel,
    cwd: &Path,
    writable_roots: &[PathBuf],
) -> Option<String> {
    let mut targets: Vec<&str> = Vec::new();
    match command {
        "rm" | "rmdir" | "unlink" | "shred" | "mkdir" | "touch" | "truncate" => {
            targets.extend(positional_operands(args));
        }
        "mv" => targets.extend(positional_operands(args)),
        "cp" | "ln" | "install" => {
            collect_option_values(args, &["-t", "--target-directory"], &mut targets);
            if targets.is_empty() {
                let operands = positional_operands(args);
                if let Some(target) = operands.last() {
                    targets.push(target);
                }
            }
        }
        "chmod" | "chown" | "chgrp" => {
            let operands = positional_operands(args);
            targets.extend(operands.into_iter().skip(1));
        }
        "dd" => {
            for arg in args {
                if let Some(target) = arg.strip_prefix("of=") {
                    targets.push(target);
                }
            }
        }
        "tee" => targets.extend(positional_operands(args)),
        "sed"
            if args.iter().any(|arg| {
                arg == "-i" || arg.starts_with("-i") || arg.starts_with("--in-place")
            }) =>
        {
            if let Some(target) = positional_operands(args).last() {
                targets.push(target);
            }
        }
        "curl" => collect_option_values(args, &["-o", "--output"], &mut targets),
        "wget" => collect_option_values(args, &["-O", "--output-document"], &mut targets),
        "sort" => collect_option_values(args, &["-o", "--output"], &mut targets),
        "find" => collect_option_values(args, &["-fprint", "-fprintf", "-fls"], &mut targets),
        "git" => {
            collect_option_values(args, &["-C", "--work-tree", "--git-dir"], &mut targets);
            if args.first().is_some_and(|arg| arg == "clone")
                && let Some(target) = positional_operands(&args[1..]).last()
                && !target.contains("://")
            {
                targets.push(target);
            }
        }
        "tar"
            if args.iter().any(|arg| {
                arg == "-x"
                    || arg == "--extract"
                    || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('x'))
            }) =>
        {
            collect_option_values(args, &["-C", "--directory"], &mut targets);
            if targets.is_empty() {
                targets.push(".");
            }
        }
        _ => return None,
    }

    if targets.is_empty() {
        return None;
    }
    if matches!(safety, SafetyLevel::High) {
        return Some(format!(
            "filesystem mutation command is not authorized at high safety: {command}"
        ));
    }
    for target in targets {
        if shell_path_is_dynamic(target) {
            if matches!(safety, SafetyLevel::Low) {
                continue;
            }
            return Some(format!(
                "dynamic mutation target is not authorized: {target}"
            ));
        }
        let resolved = match crate::tools::path::resolve_to_cwd(target, cwd) {
            Ok(path) => path,
            Err(error) => return Some(error.to_string()),
        };
        let validation = if matches!(safety, SafetyLevel::Low) {
            crate::tools::write_policy::validate_mutation_target(&resolved, cwd)
        } else {
            crate::tools::write_policy::validate_mutation_path(&resolved, cwd, writable_roots)
        };
        if let Err(error) = validation {
            return Some(error.to_string());
        }
    }
    None
}

fn positional_operands(args: &[String]) -> Vec<&str> {
    let mut operands = Vec::new();
    let mut options = true;
    for arg in args {
        if options && arg == "--" {
            options = false;
        } else if options && arg.starts_with('-') && arg != "-" {
            continue;
        } else {
            operands.push(arg.as_str());
        }
    }
    operands
}

fn collect_option_values<'a>(args: &'a [String], options: &[&str], output: &mut Vec<&'a str>) {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if options.contains(&arg.as_str())
            && let Some(value) = args.get(index + 1)
        {
            output.push(value);
            index += 2;
            continue;
        }
        if let Some((option, value)) = arg.split_once('=')
            && options.contains(&option)
        {
            output.push(value);
        }
        index += 1;
    }
}

fn shell_path_is_dynamic(path: &str) -> bool {
    path.contains(['$', '`', '*', '?', '[', '{'])
}

fn dangerous_rm(args: &[String], safety: SafetyLevel) -> bool {
    let lower_args = args
        .iter()
        .map(|arg| arg.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let has_recursive_force = lower_args.iter().any(|arg| {
        (arg.starts_with('-') && arg.contains('r') && arg.contains('f'))
            || arg == "--recursive"
            || arg == "--force"
    }) || (lower_args
        .iter()
        .any(|arg| arg == "-r" || arg == "-R" || arg == "--recursive")
        && lower_args.iter().any(|arg| arg == "-f" || arg == "--force"));
    let has_dangerous_path = lower_args.iter().any(|arg| {
        if matches!(safety, SafetyLevel::Low) {
            is_tier_independent_protected_path(arg)
        } else {
            is_sensitive_path(arg)
        }
    });
    let has_dynamic_absolute_path = lower_args
        .iter()
        .any(|arg| arg.starts_with('/') && arg.contains(['*', '?', '[']));
    has_dangerous_path
        || has_recursive_force
            && lower_args
                .iter()
                .any(|arg| is_broad_path(arg) || has_dynamic_absolute_path)
}

fn is_broad_path(arg: &str) -> bool {
    matches!(arg, "/" | "/*" | "~" | "~/" | "~/*" | "." | "./" | "./*")
}

fn is_catastrophic_path(arg: &str) -> bool {
    matches!(
        normalize_shell_path(arg).as_str(),
        "/" | "/etc"
            | "/etc/passwd"
            | "/etc/shadow"
            | "/etc/sudoers"
            | "/usr"
            | "/bin"
            | "/sbin"
            | "/boot"
            | "/sys"
            | "/proc"
            | "/dev"
            | "/lib"
            | "/lib64"
            | "/home"
            | "/var"
            | "/opt"
            | "~"
            | "~/"
    )
}

pub(super) fn is_tier_independent_protected_path(arg: &str) -> bool {
    let normalized = normalize_shell_path(arg);
    is_catastrophic_path(&normalized)
        || is_protected_credential_path(&normalized)
        || ["/dev/", "/proc/", "/sys/", "/boot/"]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

pub(super) fn is_sensitive_path(arg: &str) -> bool {
    let normalized = normalize_shell_path(arg);
    let arg = normalized.as_str();
    matches!(
        arg,
        "/" | "/etc/passwd"
            | "/etc/shadow"
            | "/etc/sudoers"
            | "/etc/hosts"
            | "/etc"
            | "/usr"
            | "/bin"
            | "/sbin"
            | "/boot"
            | "/sys"
            | "/proc"
            | "/dev"
            | "/lib"
            | "/lib64"
            | "/home"
            | "/var"
            | "/opt"
            | "~/.ssh"
            | "~/.aws"
            | "~/.vault"
            | "~/.config/ferrum"
    ) || [
        "/etc/",
        "/usr/",
        "/bin/",
        "/sbin/",
        "/boot/",
        "/sys/",
        "/proc/",
        "/dev/",
        "/lib/",
        "/lib64/",
        "/home/",
        "/var/",
        "/opt/",
        "~/.ssh",
        "~/.aws",
        "~/.vault",
        "~/.config/ferrum",
    ]
    .iter()
    .any(|prefix| arg.starts_with(prefix))
}

pub(super) fn is_protected_credential_path(arg: &str) -> bool {
    let normalized = normalize_shell_path(arg);
    let components = normalized.split('/').collect::<Vec<_>>();
    normalized.starts_with("~/.ssh")
        || normalized.starts_with("~/.aws")
        || normalized.starts_with("~/.vault")
        || normalized.starts_with("~/.config/ferrum")
        || components
            .iter()
            .any(|component| matches!(*component, ".ssh" | ".aws" | ".vault"))
        || components
            .windows(2)
            .any(|pair| matches!(pair, [".config", "ferrum"]))
}

fn normalize_shell_path(arg: &str) -> String {
    let normalized = normalize_home_prefix(arg);
    if !normalized.starts_with('/') && !normalized.starts_with("~/") {
        return normalized;
    }

    let prefix = if normalized.starts_with('/') {
        "/"
    } else {
        "~/"
    };
    let rest = normalized.strip_prefix(prefix).unwrap_or(&normalized);
    let mut components = Vec::new();
    for component in rest.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value),
        }
    }
    format!("{prefix}{}", components.join("/"))
}

fn normalize_home_prefix(arg: &str) -> String {
    let mut normalized = arg.trim_matches('"').to_string();
    for prefix in ["$home/", "${home}/", "$HOME/", "${HOME}/"] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            normalized = format!("~/{}", rest);
            break;
        }
    }
    normalized.to_ascii_lowercase()
}

fn dangerous_chmod(args: &[String], safety: SafetyLevel) -> bool {
    args.iter().any(|arg| {
        (matches!(arg.as_str(), "777" | "0777") && !matches!(safety, SafetyLevel::Low))
            || matches!(arg.as_str(), "+s" | "u+s" | "g+s")
            || arg.contains("+s")
            || octal_mode_has_special_bits(arg)
    })
}

fn octal_mode_has_special_bits(mode: &str) -> bool {
    if mode.is_empty() || !mode.chars().all(|ch| matches!(ch, '0'..='7')) {
        return false;
    }
    u32::from_str_radix(mode, 8).is_ok_and(|value| value & 0o7000 != 0)
}

fn dangerous_dd(args: &[String], safety: SafetyLevel) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix("of=").is_some_and(|target| {
            matches!(safety, SafetyLevel::High)
                || if matches!(safety, SafetyLevel::Low) {
                    is_tier_independent_protected_path(target)
                } else {
                    is_sensitive_path(&target.to_ascii_lowercase())
                }
        })
    })
}

fn dangerous_tar(args: &[String], safety: SafetyLevel) -> bool {
    if !matches!(safety, SafetyLevel::Low)
        && args.iter().any(|arg| {
            arg == "--to-command"
                || arg.starts_with("--to-command=")
                || arg == "--checkpoint-action"
                || arg.starts_with("--checkpoint-action=exec=")
        })
    {
        return true;
    }
    !matches!(safety, SafetyLevel::Low)
        && args.windows(2).any(|pair| {
            matches!(pair[0].as_str(), "--to-command" | "--checkpoint-action")
                && (pair[0] == "--to-command" || pair[1].starts_with("exec="))
        })
        || {
            let has_extract = args.iter().any(|arg| {
                arg == "-x"
                    || arg == "--extract"
                    || (arg.starts_with('-') && arg.contains('x') && !arg.starts_with("--"))
            });
            has_extract
                && if matches!(safety, SafetyLevel::Low) {
                    option_targets_tier_independent_protected_path(args, "-C")
                        || option_targets_tier_independent_protected_path(args, "--directory")
                } else {
                    option_targets_sensitive_path(args, "-C")
                        || option_targets_sensitive_path(args, "--directory")
                }
        }
}

fn dangerous_install(args: &[String], safety: SafetyLevel) -> bool {
    let has_privileged_mode = args
        .windows(2)
        .any(|pair| matches!(pair[0].as_str(), "-m" | "--mode") && is_privileged_mode(&pair[1]))
        || args.iter().any(|arg| {
            arg.strip_prefix("--mode=").is_some_and(is_privileged_mode)
                || arg.starts_with("-m") && is_privileged_mode(&arg[2..])
        });
    has_privileged_mode
        || args
            .iter()
            .any(|arg| is_tier_independent_protected_path(&arg.to_ascii_lowercase()))
        || !matches!(safety, SafetyLevel::Low) && file_operation_targets_sensitive_path(args)
}

fn is_privileged_mode(mode: &str) -> bool {
    mode.contains("+s") || octal_mode_has_special_bits(mode)
}

fn dangerous_sed(args: &[String], safety: SafetyLevel) -> bool {
    let has_in_place = args.iter().any(|arg| {
        arg == "-i"
            || arg.starts_with("-i")
            || arg == "--in-place"
            || arg.starts_with("--in-place=")
    });
    has_in_place
        && args.iter().any(|arg| {
            let arg = arg.to_ascii_lowercase();
            is_sensitive_path(&arg)
                && (!matches!(safety, SafetyLevel::Low) || is_tier_independent_protected_path(&arg))
        })
}

fn file_operation_targets_sensitive_path(args: &[String]) -> bool {
    args.iter()
        .map(|arg| arg.to_ascii_lowercase())
        .any(|arg| is_sensitive_path(&arg))
}

fn option_targets_sensitive_path(args: &[String], option: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && is_sensitive_path(&pair[1].to_ascii_lowercase()))
        || args.iter().any(|arg| {
            arg.strip_prefix(&format!("{option}="))
                .or_else(|| {
                    arg.strip_prefix(option)
                        .filter(|suffix| !suffix.is_empty() && !option.starts_with("--"))
                })
                .is_some_and(|suffix| is_sensitive_path(&suffix.to_ascii_lowercase()))
        })
}

fn option_targets_tier_independent_protected_path(args: &[String], option: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && is_tier_independent_protected_path(&pair[1]))
        || args.iter().any(|arg| {
            arg.strip_prefix(&format!("{option}="))
                .or_else(|| {
                    arg.strip_prefix(option)
                        .filter(|suffix| !suffix.is_empty() && !option.starts_with("--"))
                })
                .is_some_and(is_tier_independent_protected_path)
        })
}

fn is_shell_control_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "select"
            | "function"
            | "time"
            | "coproc"
            | "[["
            | "]]"
    )
}

fn is_dynamic_shell_builtin(command: &str) -> bool {
    matches!(
        command,
        "eval"
            | "exec"
            | "source"
            | "."
            | "alias"
            | "unalias"
            | "shopt"
            | "enable"
            | "trap"
            | "cd"
            | "pushd"
            | "popd"
    )
}

fn is_detacher_command(command: &str) -> bool {
    matches!(
        command,
        "setsid" | "nohup" | "daemonize" | "disown" | "systemd-run" | "at" | "batch"
    )
}

fn generated_code_execution(command: &str, args: &[String]) -> bool {
    matches!(command, "cc" | "gcc" | "clang" | "rustc" | "javac")
        || command == "go" && args.first().is_some_and(|arg| arg == "run")
        || command == "cargo" && args.first().is_some_and(|arg| arg == "run")
        || command == "java" && args.iter().any(|arg| arg.starts_with("/tmp/"))
}

fn is_shell_interpreter(command: &str) -> bool {
    matches!(
        command,
        "sh" | "bash" | "zsh" | "dash" | "fish" | "ksh" | "mksh" | "ash" | "csh" | "tcsh"
    )
}

fn is_inline_script_interpreter(command: &str, args: &[String]) -> bool {
    matches!(
        command,
        "python" | "python2" | "python3" | "ruby" | "perl" | "php" | "node" | "nodejs"
    ) && args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-c" | "-e" | "-r"))
}

fn is_network_tool(command: &str, args: &[String]) -> bool {
    matches!(
        command,
        "curl"
            | "wget"
            | "nc"
            | "netcat"
            | "ncat"
            | "socat"
            | "ssh"
            | "scp"
            | "rsync"
            | "ftp"
            | "sftp"
            | "tftp"
    ) || command == "git"
        && args.first().is_some_and(|arg| {
            matches!(
                arg.as_str(),
                "clone" | "fetch" | "pull" | "push" | "ls-remote" | "submodule"
            )
        })
        || command == "gh"
        || command == "openssl" && args.first().is_some_and(|arg| arg == "s_client")
}

fn contains_escape_sequence(arg: &str) -> bool {
    arg.contains("\\x") || arg.contains("\\u") || arg.contains("\\0")
}
