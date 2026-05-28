# Ferrum Roadmap

## Phase 0: Skeleton
- Create Rust crate.
- Add CLI flags.
- Add config loading.
- Add normalized message and tool types.
- Add session JSONL writer.
- Add fake provider for tests.

Done when `cargo test` passes and `ferrum --help` works.

## Phase 1: Print Mode MVP
- Implement OpenAI-compatible provider.
- Implement OpenAI Codex OAuth provider.
- Implement OpenCode Go OpenAI-compatible provider preset.
- Stream assistant text.
- Implement `read`, `ls`, `bash` tools.
- Implement provider-neutral core tool-call loop.
- Save sessions.

Done when this works:

```bash
ferrum -p "list files and explain the project"
```

## Phase 2: Tool Completeness
- Implement `write`.
- Implement `edit` with exact replacement and multi-edit support.
- Implement `grep`.
- Implement `find`.
- Add truncation policy for large outputs.
- Add tool tests.

Done when Ferrum can perform simple repo edits safely.

## Phase 3: Interactive Barebone
- Add interactive prompt loop.
- Stream assistant output live.
- Display tool calls/results.
- Add Ctrl+C abort.
- Add `/quit`, `/session`, `/model`.
- Add session continue/resume basics.

Done when Ferrum is usable as a daily local coding assistant.

## Phase 4: Additional Providers
- Add Anthropic-compatible `/messages` provider adapter for providers/models that are not Chat Completions-compatible.
- Support provider-specific streaming and tool call translation behind the normalized provider trait.
- Keep tool execution in the core loop, not provider adapters.

Done when OpenCode Go Anthropic-endpoint models, hosted Anthropic-compatible APIs, and OpenAI-compatible APIs work through the same normalized loop.

## Phase 5: Polish
- Improve terminal editor.
- Improve markdown/code rendering.
- Add AGENTS.md context loading.
- Add compact command.
- Add branch-friendly session schema behavior.
- Add packaging/release workflow.

## Deferred
- Rich TUI layout.
- Images.
- Themes.
- Extensions/plugins.
- Cross-platform support.
- OAuth.
- Package manager.
- SDK.
