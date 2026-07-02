# Colors

Ferrum uses a small semantic color palette, not a full theme system. The palette colors UI chrome and interactive display: prompt, separators, assistant text, thinking text, tool headers/results, status notices, warnings/errors, and diff previews.

Tool result content stored in session context is not modified. Raw tool output is not rewritten for the model.

## Color mode

Set the global color mode in `~/.config/ferrum/config.toml`:

```toml
color = "auto"
```

Supported modes:

```text
auto|on|off
```

- `auto`: colorize only when output is a terminal
- `on`: force ANSI color output
- `off`: disable all Ferrum UI colors

Interactive command:

```text
/colors
/colors on
/colors off
/colors auto
```

## Custom palette

Create `~/.config/ferrum/colors.toml` to override any palette entry:

```toml
prompt = "DeepSkyBlue1"
hr = "SteelBlue1"
assistant = "Grey100"
thinking = "Orange3"
tool = "bold LightSkyBlue3"
tool_output = "SkyBlue1"
status = "RoyalBlue1"
highlight = "Gold1"
success = "SpringGreen1"
warning = "Orange1"
error = "OrangeRed1"

diff_added = "SpringGreen1"
diff_removed = "DeepPink1"
diff_hunk = "MediumOrchid1"
diff_meta = "Grey70"
```

Restart Ferrum after editing `colors.toml`; the palette is loaded at startup.

You can also keep reusable palettes in `~/.config/ferrum/color-palettes/*.toml` and switch interactively:

```text
/palette
/palettes
/palette catppuccin
```

`/palette` shows the current palette, or `default`/`custom` if it does not match a named palette. `/palettes` lists available palette files. `/palette <name>` validates `color-palettes/<name>.toml`, applies it to the running session, and writes it to `~/.config/ferrum/colors.toml`.

Missing entries use defaults. Unknown palette keys or invalid color values are ignored with a warning when Ferrum starts from `colors.toml`; `/palette <name>` rejects invalid palette files before writing them.

## Palette keys

```text
prompt       ferrum> prompt
hr           horizontal separator lines
assistant    assistant response text
thinking     provider-supplied thinking/reasoning text
tool         tool call headers
tool_output  displayed tool output previews
status       status notices
highlight    highlighted labels and titles
success      successful result headers
warning      warnings
error        errors, failed result headers, bash/wait stderr previews

diff_added    inserted diff lines
diff_removed  removed diff lines
diff_hunk     unified diff hunk headers
diff_meta     diff metadata and side-by-side headers
```

Aliases:

```text
separator -> hr
rule      -> hr
```

## Color values

Ferrum accepts standard ANSI-style names, xterm 256-color names, xterm 256-color indexes, RGB hex colors, and simple styles.

Use xterm names for most custom palettes. They are stable, familiar, and avoid inventing Ferrum-specific color names. Numeric indexes remain available when a duplicated xterm name needs an exact index.

ANSI-style names:

```text
black red green yellow blue magenta purple cyan white gray grey
```

Bright ANSI-style names:

```text
bright-black bright-red bright-green bright-yellow
bright-blue bright-magenta bright-cyan bright-white
```

Xterm 256-color names are supported using the conventional xterm 256-color table names, case-insensitively. This is the xterm table with names like `DeepSkyBlue1`, `Orange3`, `SpringGreen1`, `Grey70`, and `MediumOrchid1`; it is not the full CSS/X11 color-name set.

Spaces, dashes, and underscores are ignored, and `gray`/`grey` are equivalent, so these are equivalent:

```toml
thinking = "Orange3"
thinking = "orange3"
thinking = "orange-3"
```

Examples:

```toml
prompt = "DeepSkyBlue1"
thinking = "Orange3"
tool = "bold LightSkyBlue3"
error = "OrangeRed1"
diff_added = "SpringGreen1"
diff_removed = "DeepPink1"
diff_meta = "Grey70"
```

Some xterm names are duplicated in the 256-color table. Ferrum maps a duplicate name to the first matching xterm index; use the numeric index for exact selection. For example, `Orange3` maps to index `172`, while `172` always selects that exact xterm color.

Styles:

```text
bold
dim
italic
underline
```

Styles and colors can be combined:

```toml
tool = "bold LightSkyBlue3"
thinking = "dim Orange3"
prompt = "bold DeepSkyBlue1"
```

Other formats:

```text
#ffaa00     RGB truecolor foreground
245         xterm 256-color foreground index
172         xterm Orange3
default     no explicit color/style
normal      no explicit color/style
none        no explicit color/style
off         no explicit color/style
```

Ferrum maps these values to standard ANSI escape codes. The `colors.toml` file only selects semantic UI roles; the final appearance of ANSI/xterm colors can still depend on the terminal palette.
