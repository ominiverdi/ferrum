% FERRUM(1) Ferrum User Manual
% ominiverdi
% 2026-06-05

# NAME

ferrum - small Rust-native coding agent for Linux

# SYNOPSIS

**ferrum** [OPTIONS]

**ferrum** **-p** PROMPT [OPTIONS]

**ferrum** **--resume** [REF] [OPTIONS]

**ferrum** **--continue** [OPTIONS]

**ferrum** **--session** REF [OPTIONS]

**ferrum** **login** PROVIDER

# DESCRIPTION

Ferrum is a Linux-native coding agent. It provides interactive and one-shot
prompt modes, local file and shell tools, image input, JSONL sessions,
AGENTS.md context loading, configurable providers, OpenAI Codex / ChatGPT OAuth,
OpenAI-compatible providers, Agent Skills-style instructions, and a minimal MCP
stdio bridge.

Ferrum stores sessions as JSONL files and restores context from previous
sessions when requested. It is provider-neutral: providers translate messages to
remote or local model APIs, while Ferrum owns tools, sessions, context loading,
and the agent loop.

# MODES

**Interactive mode**

: Run **ferrum** without **-p** to start an interactive session.

**Print mode**

: Use **-p**, **--print** to run one prompt and print the answer.

**Resume mode**

: Use **--resume** to resume the latest session for the current directory. If no
matching session exists, Ferrum starts a new session.

**Continue mode**

: Use **--continue** as an alias for continuing the latest session for the
current directory.

**Named or explicit session mode**

: Use **--session** REF or **--resume** REF to open a session by JSONL path or id
prefix.

# OPTIONS

**-p**, **--print** PROMPT
: Run a single prompt and print the answer. If stdin is piped, Ferrum appends it
to the prompt.

**--provider** PROVIDER
: Override the provider configured in config.toml.

**--model** MODEL
: Override the model configured in config.toml.

**--thinking** LEVEL
: Override thinking level. Supported values are **off**, **minimal**, **low**,
**medium**, **high**, and **xhigh**.

**--title** TITLE
: Set the session title.

**--image** PATH
: Attach a local image file. Repeatable. Supported formats are png, jpg, jpeg,
and webp.

**--mcp** [SERVER ...]
: Enable configured MCP servers for this process. If server names are provided,
only those servers are enabled.

**--no-mcp**
: Disable MCP servers for this process.

**--no-tools**
: Disable all tools for this process.

**--tools** TOOL ...
: Expose only the listed tools to the model.

**--resume** [REF]
: Resume the latest session for the current directory, or resume a specific
session by JSONL path or id prefix.

**--continue**
: Continue the latest session for the current directory.

**--session** REF
: Open a specific session by JSONL path or id prefix.

**-h**, **--help**
: Print command help.

**-V**, **--version**
: Print version.

# INTERACTIVE COMMANDS

Ferrum interactive mode supports slash commands and shell shortcuts.

Common slash commands:

**/help**
: Show interactive help.

**/version**
: Show Ferrum version.

**/session**
: Show current session information.

**/sessions**
: List recent sessions.

**/sessions** REF
: Open a session by number, id prefix, or path.

**/sessions pick**
: Open an interactive session picker.

**/sessions new**
: Start a new session.

**/title** TEXT
: Set session title.

**/model** NAME
: Switch model.

**/models**
: List models known to the active provider.

**/provider** NAME
: Switch provider.

**/providers**
: List configured providers.

**/mcp** on|off|status
: Manage MCP server availability for the current session.

**/thinking** LEVEL
: Set thinking level.

**/diff** MODE
: Set diff display mode.

**/skills**
: List discovered skills.

**/skill:**NAME [ARGS]
: Load a skill.

**/image** PATH
: Attach an image.

**/compact**
: Ask the model to compact session context.

**/quit**
: Exit Ferrum.

Shell shortcuts:

**!**COMMAND
: Run a shell command and send its output to the model.

**!!**COMMAND
: Run a shell command and show the output only to the user.

# CONFIGURATION

Ferrum reads configuration from:

**$XDG_CONFIG_HOME/ferrum/config.toml**

or, if XDG_CONFIG_HOME is unset:

**~/.config/ferrum/config.toml**

A minimal OpenAI-compatible provider configuration looks like:

```toml
provider = "local"
model = "local-model"

[providers.local]
type = "openai-compatible"
base_url = "http://localhost:8080"
default_model = "local-model"
```

Providers that require keys should use environment variables rather than
hardcoded secrets:

```toml
[providers.example]
type = "openai-compatible"
base_url = "https://api.example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
```

# FILES

**~/.config/ferrum/config.toml**
: User configuration.

**~/.local/share/ferrum/sessions/**
: JSONL session store, unless data directory is overridden by XDG variables.

**AGENTS.md**
: Project and user instruction files loaded from the current directory tree and
configuration directory.

# INSTALLATION

## Release binary

Download the release tarball from Codeberg, extract it, and install the binary:

```sh
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.18/ferrum-v0.4.18-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install -Dm755 ferrum-v0.4.18-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/ferrum
ferrum --help
```

Optional checksum verification:

```sh
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.18/ferrum-v0.4.18-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.18/ferrum-v0.4.18-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.4.18-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## From source

Install with Cargo:

```sh
git clone https://codeberg.org/ominiverdi/ferrum.git
cd ferrum
cargo install --path .
ferrum --help
```

## System-wide man page

If the release or source checkout includes **docs/ferrum.1**, install it with:

```sh
sudo install -Dm644 docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
mandb 2>/dev/null || true
man ferrum
```

If only **docs/ferrum.1.md** is present, generate the man page with pandoc:

```sh
pandoc -s -t man docs/ferrum.1.md -o docs/ferrum.1
```

# EXAMPLES

Run a one-shot prompt:

```sh
ferrum -p "summarize this repo"
```

Pipe input into a prompt:

```sh
cat src/main.rs | ferrum -p "review this file"
```

Use a local OpenAI-compatible provider:

```sh
ferrum --provider local -p "say hello"
```

Resume the latest session for the current directory:

```sh
ferrum --resume
```

Attach an image:

```sh
ferrum --image ./screenshot.png -p "describe this image"
```

Limit tool exposure:

```sh
ferrum --tools read grep find -p "inspect this repo"
ferrum --no-tools -p "answer without tools"
```

# SEE ALSO

**cargo**(1), **git**(1), **pandoc**(1)

Project repository: https://codeberg.org/ominiverdi/ferrum
