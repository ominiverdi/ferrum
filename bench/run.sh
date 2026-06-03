#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: bench/run.sh <agent-profile> <task>" >&2
  echo "profiles: ferrum-codex, ferrum-codex-bash, pi-codex, opencode" >&2
  exit 2
fi

agent="$1"
task="$2"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
task_dir="$root/bench/tasks/$task"
if [ ! -d "$task_dir" ]; then
  echo "unknown task: $task" >&2
  exit 2
fi

stamp="$(date +%Y%m%d-%H%M%S)"
runs_root="${BENCH_RUN_ROOT:-/tmp/ferrum-bench-runs}"
run_dir="$runs_root/${stamp}-${agent}-${task}"
work_dir="$run_dir/work"
mkdir -p "$work_dir"
cp "$task_dir/prompt.md" "$run_dir/prompt.md"

(
  cd "$work_dir"
  bash "$task_dir/setup.sh"
  git init -q
  git add .
  git commit --allow-empty -qm baseline
)

prompt="$(cat "$run_dir/prompt.md")"
cmd_file="$run_dir/command.txt"
case "$agent" in
  ferrum-codex)
    model="${FERRUM_MODEL:-gpt-5.5}"
    printf 'ferrum --no-mcp --provider openai-codex --model %q -p <prompt>\n' "$model" > "$cmd_file"
    ;;
  ferrum-codex-bash)
    model="${FERRUM_MODEL:-gpt-5.5}"
    printf 'ferrum --no-mcp --tools bash --provider openai-codex --model %q -p <prompt>\n' "$model" > "$cmd_file"
    ;;
  pi-codex)
    model="${PI_MODEL:-gpt-5.5}"
    pi_tools="${PI_TOOLS:-read,bash,edit,write,grep,find,ls}"
    printf 'pi --provider openai-codex --model %q --no-session --tools %q -p <prompt>\n' "$model" "$pi_tools" > "$cmd_file"
    ;;
  opencode)
    model="${OPENCODE_MODEL:?set OPENCODE_MODEL, e.g. OPENCODE_MODEL=provider/model}"
    printf 'opencode run --model %q <prompt>\n' "$model" > "$cmd_file"
    ;;
  *)
    echo "unknown agent profile: $agent" >&2
    exit 2
    ;;
esac

exit_code=0
(
  cd "$work_dir"
  case "$agent" in
    ferrum-codex)
      if [ "${FERRUM_OFFLINE:-}" = "1" ] || [ "${FERRUM_OFFLINE:-}" = "true" ]; then
        /usr/bin/time -v -o "$run_dir/time.txt" ferrum --no-mcp --model fake -p "$prompt" \
          > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      else
        /usr/bin/time -v -o "$run_dir/time.txt" ferrum --no-mcp --provider openai-codex --model "$model" -p "$prompt" \
          > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      fi
      ;;
    ferrum-codex-bash)
      if [ "${FERRUM_OFFLINE:-}" = "1" ] || [ "${FERRUM_OFFLINE:-}" = "true" ]; then
        /usr/bin/time -v -o "$run_dir/time.txt" ferrum --no-mcp --tools bash --model fake -p "$prompt" \
          > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      else
        /usr/bin/time -v -o "$run_dir/time.txt" ferrum --no-mcp --tools bash --provider openai-codex --model "$model" -p "$prompt" \
          > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      fi
      ;;
    pi-codex)
      /usr/bin/time -v -o "$run_dir/time.txt" pi --provider openai-codex --model "$model" --no-session --tools "$pi_tools" -p "$prompt" \
        > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      ;;
    opencode)
      /usr/bin/time -v -o "$run_dir/time.txt" opencode run --model "$model" "$prompt" \
        > "$run_dir/stdout.txt" 2> "$run_dir/stderr.txt"
      ;;
  esac
) || exit_code=$?

(
  cd "$work_dir"
  git add -N . >/dev/null 2>&1 || true
  git diff -- . > "$run_dir/diff.patch" || true
)

validate_code=0
(
  cd "$work_dir"
  bash "$task_dir/validate.sh"
) > "$run_dir/validate.txt" 2>&1 || validate_code=$?

max_rss_kb="$(awk -F: '/Maximum resident set size/ {gsub(/^[ \t]+/, "", $2); print $2}' "$run_dir/time.txt" || true)"
elapsed="$(sed -n 's/^\tElapsed (wall clock) time (h:mm:ss or m:ss): //p' "$run_dir/time.txt" || true)"
cat > "$run_dir/result.env" <<RESULT
agent=$agent
task=$task
run_dir=$run_dir
exit_code=$exit_code
validate_code=$validate_code
score_code=pending
max_rss_kb=$max_rss_kb
elapsed=$elapsed
RESULT

score_code=0
if [ -x "$task_dir/score.sh" ]; then
  bash "$task_dir/score.sh" "$run_dir" > "$run_dir/score.txt" 2>&1 || score_code=$?
else
  printf 'no score.sh\n' > "$run_dir/score.txt"
fi
perl -0pi -e "s/^score_code=.*/score_code=$score_code/m" "$run_dir/result.env"

cat "$run_dir/result.env"
if [ "$exit_code" -ne 0 ] || [ "$validate_code" -ne 0 ] || [ "$score_code" -ne 0 ]; then
  exit 1
fi
