# Ferrum Agent Instructions

## Project
Ferrum is a Linux-only, Rust-native, small, fast coding agent inspired by Pi.

Ferrum is a new project, not a compatibility port.

## Repository / Forge Situation
- Codeberg is the primary source repository:
  `https://codeberg.org/ominiverdi/ferrum`
- In the primary local clone:
  - `origin` must point to Codeberg: `ssh://git@codeberg.org/ominiverdi/ferrum.git`
  - `github` must point to the GitHub mirror: `git@github.com:ominiverdi/ferrum.git`
- GitHub remains a mirror repository:
  `https://github.com/ominiverdi/ferrum`
- Do not use stale GitHub-only working copies for new work unless explicitly asked.

## Publishing / Release Rules
- Implement changes locally first.
- Run local validation before publishing:
  ```bash
  cargo fmt --check
  cargo test
  cargo build --release
  ```
- Wait for explicit user approval before pushing, tagging, creating releases, uploading assets, or otherwise publishing.
- Never publish untested or user-unapproved code.
- Normal source push after approval:
  ```bash
  git push origin main
  git push github main
  ```
- Normal tagged release:
  ```bash
  git tag -a vX.Y.Z -F /tmp/ferrum-vX.Y.Z-notes.md
  git push origin main vX.Y.Z
  git push github main vX.Y.Z
  ```
- Create the Codeberg release locally with `tea` and upload locally built assets. This is the primary and preferred release path.
- GitHub tag push may still trigger mirror release automation if configured, but Codeberg is the primary release host.

## Codeberg collaboration

- Prefer Codeberg for issues, pull requests, and releases.
- When asked to inspect Codeberg issues or PRs, use `tea` first.
- For non-interactive issue creation on Codeberg, use:
  `tea issues create --repo ominiverdi/ferrum --login codeberg.org ...`
- If `tea` comment/review flows require an interactive terminal, draft the exact reply for the user instead of claiming it was posted.

## Goals
- Native Linux CLI/TUI coding agent.
- Small, fast, predictable.
- Minimal dependencies where practical.
- Rust-native runtime, no TypeScript runtime.
- Provider-neutral core loop and tools.
- Useful Pi concepts only: agent loop, tools, sessions, context files, skills-like instructions.

## Current Scope
- Rust stable.
- Interactive and print modes.
- JSONL sessions with resume/switching/metadata.
- OpenAI Codex / ChatGPT OAuth provider.
- OpenAI-compatible providers for remote APIs and local servers.
- MCP stdio bridge.
- Image input.
- Agent Skills-style instruction packages.
- Native tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`.

## Non-goals for v1
- Cross-platform support beyond Linux.
- TypeScript extensions.
- SDK compatibility with Pi.
- Pi package manager compatibility.
- Themes.
- Rich plugin UI.
- Auto-update checks.

## Engineering Rules
- Prefer simple Rust modules over abstractions until duplication hurts.
- Keep provider adapters thin and explicit.
- Keep tools provider-neutral; providers only translate requests/responses.
- Avoid speculative generalization.
- Preserve session data with stable JSONL schemas.
- Treat tool execution and file mutation as critical correctness paths.
- Favor deterministic behavior and clear errors over magic.
- Do not hardcode secrets.
- Read API keys from environment/config/OAuth storage only.
- Never commit secrets, tokens, OAuth credentials, Vault material, local sessions, or generated artifacts.
- Keep README concise; put details in `docs/`.

## Initial Stack / Dependency Bias
- `tokio` for async runtime.
- `clap` for CLI.
- `serde` / `serde_json` for data.
- `reqwest` for HTTP.
- `crossterm` for terminal control.
- `ignore`, `walkdir`, `globset` for filesystem traversal.
- `similar` for diffs.
- `anyhow` and `thiserror` for errors.
- Add dependencies only when they clearly simplify correctness or maintenance.

## Codeberg Tooling
- `tea` can be used for Codeberg operations when configured locally.
- SSH auth to Codeberg should be configured locally before pushing.
- Use `git` for normal push/fetch/tag operations.
- Use `tea` for Codeberg repo, issue, PR, and release operations when needed.

## Style
- Keep code readable and boring.
- Small files, clear module boundaries.
- No icons or emoticons in logs, comments, docs, or UI strings.
- Plain terminal output; no color dependency for core rendering.
