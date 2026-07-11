#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ "$#" -ne 2 ]]; then
  echo "usage: bench/run.sh <agent-profile> <task>" >&2
  echo "profiles: ferrum-codex, ferrum-codex-bash, pi-codex, opencode" >&2
  exit 2
fi

agent="$1"
task="$2"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$agent" in
  ferrum-codex|ferrum-codex-bash|pi-codex|opencode) ;;
  *) echo "unknown agent profile: $agent" >&2; exit 2 ;;
esac
if ! [[ "$task" =~ ^[0-9]{3}-[a-z0-9]+(-[a-z0-9]+)*$ ]]; then
  echo "invalid task ID: $task" >&2
  exit 2
fi

tasks_root="$(realpath -e -- "$root/bench/tasks")"
if ! task_dir="$(realpath -e -- "$tasks_root/$task" 2>/dev/null)" || [[ ! -d "$task_dir" ]]; then
  echo "unknown task: $task" >&2
  exit 2
fi
case "$task_dir/" in
  "$tasks_root"/*/) ;;
  *) echo "task escapes benchmark root: $task" >&2; exit 2 ;;
esac
for required in prompt.md setup.sh validate.sh; do
  [[ -f "$task_dir/$required" && ! -L "$task_dir/$required" ]] || {
    echo "invalid benchmark task file: $task/$required" >&2
    exit 2
  }
done

validate_timeout() {
  local name="$1" value="$2"
  if ! [[ "$value" =~ ^[1-9][0-9]*$ ]] || (( value > 86400 )); then
    echo "$name must be an integer from 1 through 86400" >&2
    exit 2
  fi
}
validate_dedicated_home() {
  local path="$1" mode
  [[ -d "$path" && ! -L "$path" && -O "$path" ]] || {
    echo "BENCH_AGENT_HOME must be an owned, non-symlink directory" >&2
    return 1
  }
  mode="$(stat -c '%a' "$path")"
  (( (8#$mode & 077) == 0 )) || {
    echo "BENCH_AGENT_HOME must not be accessible by group or other users" >&2
    return 1
  }
  realpath -e -- "$path"
}
setup_timeout="${BENCH_SETUP_TIMEOUT_SECONDS:-120}"
agent_timeout="${BENCH_AGENT_TIMEOUT_SECONDS:-900}"
validate_timeout_seconds="${BENCH_VALIDATE_TIMEOUT_SECONDS:-120}"
score_timeout="${BENCH_SCORE_TIMEOUT_SECONDS:-120}"
validate_timeout BENCH_SETUP_TIMEOUT_SECONDS "$setup_timeout"
validate_timeout BENCH_AGENT_TIMEOUT_SECONDS "$agent_timeout"
validate_timeout BENCH_VALIDATE_TIMEOUT_SECONDS "$validate_timeout_seconds"
validate_timeout BENCH_SCORE_TIMEOUT_SECONDS "$score_timeout"

runs_root="${BENCH_RUN_ROOT:-${XDG_RUNTIME_DIR:-/tmp}/ferrum-bench-runs-${UID}}"
case "$runs_root" in *$'\n'*|*$'\r'*) echo "invalid benchmark run root" >&2; exit 2 ;; esac
if [[ -L "$runs_root" ]]; then
  echo "benchmark run root may not be a symlink: $runs_root" >&2
  exit 2
fi
install -d -m 0700 "$runs_root"
[[ -O "$runs_root" ]] || { echo "benchmark run root is not owned by current user" >&2; exit 2; }
chmod 0700 "$runs_root"
runs_root="$(realpath -e -- "$runs_root")"
run_dir="$(mktemp -d "$runs_root/run.${agent}.${task}.XXXXXXXXXX")"
chmod 0700 "$run_dir"
work_dir="$run_dir/work"
config_dir="$run_dir/config"
data_dir="$run_dir/data"
run_home="$run_dir/home"
mkdir -m 0700 "$work_dir" "$config_dir" "$data_dir" "$run_home"
install -m 0600 "$task_dir/prompt.md" "$run_dir/prompt.md"
harness_env=(
  "HOME=$run_home"
  "PATH=$PATH"
  "LANG=C.UTF-8"
  "LC_ALL=C.UTF-8"
  "TERM=dumb"
)

setup_code=0
(
  cd "$work_dir"
  env -i "${harness_env[@]}" \
    timeout --signal=TERM --kill-after=10s "${setup_timeout}s" bash "$task_dir/setup.sh"
) > "$run_dir/setup.txt" 2>&1 || setup_code=$?
if [[ "$setup_code" -ne 0 ]]; then
  printf 'setup failed with status %s; run_dir=%s\n' "$setup_code" "$run_dir" >&2
  exit 1
fi
(
  cd "$work_dir"
  env -i "${harness_env[@]}" git init -q
  env -i "${harness_env[@]}" git config user.name "Ferrum benchmark"
  env -i "${harness_env[@]}" git config user.email "benchmark@invalid"
  env -i "${harness_env[@]}" git add .
  env -i "${harness_env[@]}" git commit --allow-empty -qm baseline
)

prompt="$(cat "$run_dir/prompt.md")"
cmd_file="$run_dir/command.txt"
model=""
provider=""
tools=""
offline="${FERRUM_OFFLINE:-0}"
agent_executable=""
agent_version=""
profile_home_mode="isolated"

auth_copy=""
cleanup_auth() {
  if [[ -n "$auth_copy" ]]; then
    rm -f "$auth_copy"
  fi
}
trap cleanup_auth EXIT

case "$agent" in
  ferrum-codex|ferrum-codex-bash)
    agent_executable="$(command -v ferrum || true)"
    [[ -n "$agent_executable" ]] || { echo "ferrum executable not found" >&2; exit 1; }
    agent_executable="$(realpath -e -- "$agent_executable")"
    model="${FERRUM_MODEL:-gpt-5.5}"
    provider="openai-codex"
    if [[ "$agent" == "ferrum-codex-bash" ]]; then
      tools="bash"
    else
      tools="default"
    fi
    if [[ "$offline" != "1" && "$offline" != "true" ]]; then
      source_config="${FERRUM_CONFIG_DIR:-$HOME/.config/ferrum}"
      auth_source="${BENCH_FERRUM_AUTH_FILE:-$source_config/auth.json}"
      [[ -f "$auth_source" && ! -L "$auth_source" ]] || {
        echo "Ferrum benchmark auth file not found; set BENCH_FERRUM_AUTH_FILE" >&2
        exit 1
      }
      auth_copy="$config_dir/auth.json"
      install -m 0600 "$auth_source" "$auth_copy"
    fi
    ;;
  pi-codex)
    agent_executable="$(command -v pi || true)"
    [[ -n "$agent_executable" ]] || { echo "pi executable not found" >&2; exit 1; }
    agent_executable="$(realpath -e -- "$agent_executable")"
    model="${PI_MODEL:-gpt-5.5}"
    provider="openai-codex"
    tools="${PI_TOOLS:-read,bash,edit,write,grep,find,ls}"
    [[ -n "${BENCH_AGENT_HOME:-}" ]] || {
      echo "pi-codex requires BENCH_AGENT_HOME pointing to a dedicated benchmark home" >&2
      exit 2
    }
    run_home="$(validate_dedicated_home "$BENCH_AGENT_HOME")"
    profile_home_mode="explicit-dedicated"
    ;;
  opencode)
    agent_executable="$(command -v opencode || true)"
    [[ -n "$agent_executable" ]] || { echo "opencode executable not found" >&2; exit 1; }
    agent_executable="$(realpath -e -- "$agent_executable")"
    model="${OPENCODE_MODEL:?set OPENCODE_MODEL, e.g. OPENCODE_MODEL=provider/model}"
    provider="${model%%/*}"
    tools="agent-default"
    [[ -n "${BENCH_AGENT_HOME:-}" ]] || {
      echo "opencode requires BENCH_AGENT_HOME pointing to a dedicated benchmark home" >&2
      exit 2
    }
    run_home="$(validate_dedicated_home "$BENCH_AGENT_HOME")"
    profile_home_mode="explicit-dedicated"
    ;;
esac

agent_sha256="$(sha256sum "$agent_executable" | cut -d' ' -f1)"
agent_version="$(timeout 10s "$agent_executable" --version 2>&1 | head -n1 || true)"
agent_version="${agent_version//$'\n'/ }"
agent_version="${agent_version//$'\r'/ }"
source_commit="$(git -C "$root" rev-parse HEAD 2>/dev/null || printf unknown)"
task_sha256="$(find "$task_dir" -maxdepth 1 -type f -print0 | sort -z \
  | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)"
cat > "$run_dir/provenance.txt" <<EOF
agent_profile=$agent
agent_executable=$agent_executable
agent_sha256=$agent_sha256
agent_version=$agent_version
provider=$provider
model=$model
tools=$tools
profile_home_mode=$profile_home_mode
ferrum_source_commit=$source_commit
task=$task
task_sha256=$task_sha256
setup_timeout_seconds=$setup_timeout
agent_timeout_seconds=$agent_timeout
validate_timeout_seconds=$validate_timeout_seconds
score_timeout_seconds=$score_timeout
environment=HOME,PATH,LANG,LC_ALL,TERM,FERRUM_CONFIG_DIR,FERRUM_DATA_DIR,FERRUM_FAKE_SCRIPT
EOF

common_env=(
  "HOME=$run_home"
  "PATH=$PATH"
  "LANG=C.UTF-8"
  "LC_ALL=C.UTF-8"
  "TERM=dumb"
)
agent_cmd=()
case "$agent" in
  ferrum-codex)
    common_env+=("FERRUM_CONFIG_DIR=$config_dir" "FERRUM_DATA_DIR=$data_dir")
    if [[ "$offline" == "1" || "$offline" == "true" ]]; then
      common_env+=("FERRUM_FAKE_SCRIPT=${FERRUM_FAKE_SCRIPT:-single_response}")
      agent_cmd=("$agent_executable" --no-mcp --model fake -p "$prompt")
      printf '%q ' "$agent_executable" --no-mcp --model fake -p '<prompt>' > "$cmd_file"
    else
      agent_cmd=("$agent_executable" --no-mcp --provider "$provider" --model "$model" -p "$prompt")
      printf '%q ' "$agent_executable" --no-mcp --provider "$provider" --model "$model" -p '<prompt>' > "$cmd_file"
    fi
    ;;
  ferrum-codex-bash)
    common_env+=("FERRUM_CONFIG_DIR=$config_dir" "FERRUM_DATA_DIR=$data_dir")
    if [[ "$offline" == "1" || "$offline" == "true" ]]; then
      common_env+=("FERRUM_FAKE_SCRIPT=${FERRUM_FAKE_SCRIPT:-single_response}")
      agent_cmd=("$agent_executable" --no-mcp --tools bash --model fake -p "$prompt")
      printf '%q ' "$agent_executable" --no-mcp --tools bash --model fake -p '<prompt>' > "$cmd_file"
    else
      agent_cmd=("$agent_executable" --no-mcp --tools bash --provider "$provider" --model "$model" -p "$prompt")
      printf '%q ' "$agent_executable" --no-mcp --tools bash --provider "$provider" --model "$model" -p '<prompt>' > "$cmd_file"
    fi
    ;;
  pi-codex)
    agent_cmd=("$agent_executable" --provider "$provider" --model "$model" --no-session --tools "$tools" -p "$prompt")
    printf '%q ' "$agent_executable" --provider "$provider" --model "$model" --no-session --tools "$tools" -p '<prompt>' > "$cmd_file"
    ;;
  opencode)
    agent_cmd=("$agent_executable" run --model "$model" "$prompt")
    printf '%q ' "$agent_executable" run --model "$model" '<prompt>' > "$cmd_file"
    ;;
esac
printf '\n' >> "$cmd_file"

exit_code=0
(
  cd "$work_dir"
  env -i "${common_env[@]}" \
    /usr/bin/time -v -o "$run_dir/time.txt" \
    timeout --signal=TERM --kill-after=10s "${agent_timeout}s" \
    "${agent_cmd[@]}" \
    > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
) || exit_code=$?
cleanup_auth
auth_copy=""

(
  cd "$work_dir"
  env -i "${harness_env[@]}" git add -N . >/dev/null 2>&1 || true
  env -i "${harness_env[@]}" git diff -- . > "$run_dir/diff.patch" || true
)

validate_code=0
(
  cd "$work_dir"
  env -i "${harness_env[@]}" \
    timeout --signal=TERM --kill-after=10s "${validate_timeout_seconds}s" \
      bash "$task_dir/validate.sh"
) > "$run_dir/validate.txt" 2>&1 || validate_code=$?

score_code=0
if [[ -f "$task_dir/score.sh" && ! -L "$task_dir/score.sh" ]]; then
  env -i "${harness_env[@]}" \
    timeout --signal=TERM --kill-after=10s "${score_timeout}s" \
      bash "$task_dir/score.sh" "$run_dir" > "$run_dir/score.txt" 2>&1 || score_code=$?
else
  printf 'no score.sh\n' > "$run_dir/score.txt"
fi

max_rss_kb="$(awk -F: '/Maximum resident set size/ {gsub(/^[ \t]+/, "", $2); print $2}' "$run_dir/time.txt" || true)"
elapsed="$(sed -n 's/^\tElapsed (wall clock) time (h:mm:ss or m:ss): //p' "$run_dir/time.txt" || true)"
[[ "$exit_code" -eq 124 || "$exit_code" -eq 137 ]] && agent_timed_out=1 || agent_timed_out=0
[[ "$validate_code" -eq 124 || "$validate_code" -eq 137 ]] && validate_timed_out=1 || validate_timed_out=0
[[ "$score_code" -eq 124 || "$score_code" -eq 137 ]] && score_timed_out=1 || score_timed_out=0
cat > "$run_dir/result.env" <<RESULT
agent=$agent
task=$task
run_dir=$run_dir
exit_code=$exit_code
validate_code=$validate_code
score_code=$score_code
agent_timed_out=$agent_timed_out
validate_timed_out=$validate_timed_out
score_timed_out=$score_timed_out
max_rss_kb=$max_rss_kb
elapsed=$elapsed
agent_sha256=$agent_sha256
task_sha256=$task_sha256
source_commit=$source_commit
RESULT

cat "$run_dir/result.env"
if [[ "$exit_code" -ne 0 || "$validate_code" -ne 0 || "$score_code" -ne 0 ]]; then
  exit 1
fi
