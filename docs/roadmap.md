# Ferrum Roadmap

Ferrum is an early Linux-native Rust coding agent. This roadmap tracks shipped work and likely next steps; it is not a compatibility plan for Pi.

## Shipped

### v0.1-v0.2: Core agent
- Rust crate, CLI, config loading, and provider/model overrides.
- Print mode with provider-neutral tool loop.
- JSONL sessions and resume.
- AGENTS.md context loading.
- Built-in tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`.
- Safer file/tool behavior: path normalization, exact edit validation, output truncation.

### v0.3: Interactive, MCP, images
- Interactive REPL with history and slash commands.
- Context budget tracking and compaction.
- Thinking levels.
- MCP stdio tool bridge.
- Image input via CLI, `/image`, and `/paste-image`.
- Terminal image previews when supported.
- Release and CI workflows.

### v0.4: Providers, live models, skills
- Config-backed provider registry with `[providers.<name>]`.
- `/providers`, `/provider`, and live `/models`.
- Live OpenAI Codex / ChatGPT model catalog discovery.
- OpenAI-compatible providers for remote APIs and local servers.
- Agent Skills discovery and Pi-style `/skill:<name> [args]` expansion.
- Pi-like shell shortcuts: `!<cmd>` and `!!<cmd>`.
- Runtime self-awareness context.
- Current-directory session picker/switching with `/sessions`.
- Model-generated compaction with recent-context retention.
- Plain multiline tool rendering, bounded tool-result previews, and unified diff-style `edit` rendering.
- Final no-tools synthesis when the per-turn tool-round budget is exhausted.
- Lowercase `agents.md` context loading alongside `AGENTS.md`.

## Next

### Provider improvements
- Add Anthropic-compatible `/messages` adapter for providers/models that are not Chat Completions-compatible.
- Add provider-specific compatibility flags only when verified by real provider behavior.
- Consider provider/model validation when switching providers or setting `/model`.
- Improve `/models` errors and provider-specific quirks.

### Interactive UX
- Improve multiline prompt editing/history behavior.
- Add `/images` and `/clear-images` for pending image state.
- Improve model/provider selection UX without adding a heavy TUI.
- Add clearer status output for auth/config problems.

### Sessions and context
- Improve compaction quality and summarization control.
- Add safer session branching/fork behavior.
- Keep JSONL schemas stable and documented.

### Skills
- Add tests around project/global precedence and direct `.md` discovery.
- Improve skill error messages.
- Consider optional skill asset/reference helpers without auto-executing code.

## Deferred

- Rich TUI layout.
- Themes.
- Pi extension/plugin compatibility.
- Package manager.
- SDK compatibility.
- Cross-platform support beyond Linux.
- Auto-update checks.
