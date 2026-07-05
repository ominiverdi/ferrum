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
prompt modes, local file and guarded shell tools, model-facing session history lookup,
image input, JSONL sessions,
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

**Print-mode named session mode**

: Use **--session** REF with **-p** to resume or create a named print-mode
session. If REF is a valid session id and no matching session exists, Ferrum
creates **REF.jsonl** in the session data directory.

**Interactive explicit session mode**

: Use **--session** REF or **--resume** REF in interactive mode to open an
existing session by JSONL path or id prefix.

**Continue mode**

: Use **--continue** as an alias for continuing the latest session for the
current directory.

# OPTIONS

**-p**, **--print** [PROMPT]
: Run a single prompt and print the answer. If PROMPT is omitted, Ferrum reads
it from stdin. If stdin is piped with a PROMPT, Ferrum appends stdin to the
prompt.

**--provider** PROVIDER
: Override the provider configured in config.toml.

**--model** MODEL
: Override the model configured in config.toml.

**--thinking** LEVEL
: Override thinking level. Supported values are **off**, **minimal**, **low**,
**medium**, **high**, and **xhigh**.

**--safety** LEVEL
: Override shell safety level for this process. Supported values are **low**,
**medium**, and **high**.

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

# MODEL TOOLS

Ferrum's default native tool set includes:

**read**, **write**, **edit**, **bash**, **wait**, **grep**, **find**, **ls**,
**history_search**, and **history_read**.

The `bash` and `wait` tools apply a safety-tiered shell guard before execution.
Use **--safety low|medium|high** at startup or **/safety low|medium|high**
interactively to choose the trade-off. The default **medium** tier rejects
destructive patterns and rewriteable opaque shell syntax while allowing common
coding commands such as Python one-liners and network tools.

The history tools are model-facing only. They search or read the current session
JSONL, including entries archived before compaction, and return rendered text
with JSONL line numbers. There is no slash command for these tools.

**--resume** [REF]
: Resume the latest session for the current directory, or resume a specific
session by JSONL path or id prefix.

**--continue**
: Continue the latest session for the current directory.

**--session** REF
: In print mode, resume or create a named session. In interactive mode, open an
existing session by JSONL path or id prefix. Valid named session ids use 1-80
characters from A-Z, a-z, 0-9, '.', '_', or '-', and must not start with '.'.

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

**/sessions pick**
: Open an interactive session picker.

**/sessions del**
: Open an interactive session deletion picker.

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

**/mcp** on|off|status|list
: Manage MCP server availability for the current session.

**/thinking** LEVEL
: Set thinking level.

**/safety** LEVEL
: Set shell safety level.

**/diff** MODE
: Set diff display mode.

**/colors** MODE
: Set color mode.

**/palette** [NAME]
: Show the current palette, or validate, apply, and persist a palette to **~/.config/ferrum/colors.toml**.

**/palettes**
: List palettes from **~/.config/ferrum/color-palettes/**.

**/usage** [day|week|month]
: Show token usage summary.

**/skills**
: List discovered skills.

**/skill** NAME [ARGS]
: Load a skill.

**/skill:**NAME [ARGS]
: Load a skill.

**/image** PATH
: Attach an image.

**/image-paste**
: Attach an image from the clipboard.

**/paste-image**
: Attach an image from the clipboard.

**/compact**
: Ask the model to compact session context.

**/quit**, **/exit**
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

Ferrum supports a small semantic UI color palette. Set the color mode in
**config.toml**:

```toml
color = "auto"
```

Supported modes are **auto**, **on**, and **off**. Override palette entries in
**~/.config/ferrum/colors.toml**:

```toml
prompt = "DeepSkyBlue1"
tool = "bold LightSkyBlue3"
error = "OrangeRed1"
diff_added = "SpringGreen1"
diff_removed = "DeepPink1"
```

Supported color values include ANSI-style names such as **red**, **green**, and
**cyan**, bright names such as **bright-red**, xterm 256-color table names such
as **Orange3** and **DeepSkyBlue1**, styles such as **bold** and **dim**, RGB
hex values such as **#ffaa00**, and xterm 256-color indexes such as **245**.
Xterm names are matched case-insensitively. Spaces, dashes, and underscores are
ignored, and **gray**/**grey** are equivalent. Duplicate xterm names map to the
first matching xterm index; use numeric indexes for exact selection. Reusable
palettes can live in **~/.config/ferrum/color-palettes/*.toml**; **/palette**
shows the current palette, **/palettes** lists palette files, and **/palette
NAME** validates and applies one live. See **docs/colors.md** for all palette
keys and color values.

# FILES

**~/.config/ferrum/config.toml**
: User configuration.

**~/.config/ferrum/colors.toml**
: Optional semantic UI color palette.

**~/.config/ferrum/color-palettes/*.toml**
: Optional reusable UI palettes selectable with **/palette** and listed with **/palettes**.

**~/.local/share/ferrum/sessions/**
: JSONL session store, unless data directory is overridden by XDG variables.

**AGENTS.md**
: Project and user instruction files loaded from the current directory tree and
configuration directory.

# INSTALLATION

## Release binary

Download the release tarball from Codeberg, extract it, and install the binary and man page:

```sh
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.1/ferrum-v0.6.1-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install -Dm755 ferrum-v0.6.1-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/ferrum
sudo install -Dm644 ferrum-v0.6.1-x86_64-unknown-linux-gnu/docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
sudo mandb 2>/dev/null || true
ferrum --help
man ferrum
```

Optional checksum verification:

```sh
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.1/ferrum-v0.6.1-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.1/ferrum-v0.6.1-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.6.1-x86_64-unknown-linux-gnu.tar.gz.sha256
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

If a release tarball was extracted, the main install command above installs the included man page. From a source checkout, install **docs/ferrum.1** with:

```sh
sudo install -Dm644 docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
sudo mandb 2>/dev/null || true
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
