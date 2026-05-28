# Ferrum Agent Instructions

## Project
Ferrum is a Linux-only, Rust-native, barebone, fast coding agent inspired by Pi.

This is a new project, not a compatibility port.

## Goals
- Native Linux CLI/TUI coding agent.
- Small, fast, predictable.
- Minimal dependencies where practical.
- No TypeScript runtime.
- No extension system in v1.
- No npm/package ecosystem compatibility.
- Preserve useful Pi concepts only: agent loop, tools, sessions, context files.

## Non-goals for v1
- Cross-platform support beyond Linux.
- TypeScript extensions.
- SDK compatibility with Pi.
- Package manager.
- OAuth login flows.
- Themes.
- Rich plugin UI.
- Image support.
- Auto-update checks.

## Engineering Rules
- Prefer simple Rust modules over abstractions until duplication hurts.
- Keep provider adapters thin and explicit.
- Avoid speculative generalization.
- Preserve session data with stable JSONL schemas.
- Treat tool execution and file mutation as critical correctness paths.
- Favor deterministic behavior and clear errors over magic.
- Do not hardcode secrets.
- Read API keys from environment/config only.

## Initial Stack
- Rust stable.
- `tokio` for async runtime.
- `clap` for CLI.
- `serde` / `serde_json` for data.
- `reqwest` for HTTP.
- `crossterm` initially for terminal control.
- Add `ratatui` only if/when the interactive UI needs it.
- `ignore`, `walkdir`, `globset` for filesystem traversal.
- `similar` for diffs if needed.
- `anyhow` and `thiserror` for errors.

## v1 Provider Scope
- Anthropic API.
- OpenAI-compatible Chat Completions or Responses API.
- Add more providers only after the core loop is stable.

## v1 Tool Scope
- `read`
- `write`
- `edit`
- `bash`
- `grep`
- `find`
- `ls`

## Style
- Keep code readable and boring.
- Small files, clear module boundaries.
- No icons or emoticons in logs, comments, or UI strings.
