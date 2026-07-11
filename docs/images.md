# Images

Ferrum supports local image attachments for providers that accept multimodal input.

Status: early feature.

## Supported input formats

- PNG
- JPEG
- WebP

Per-image decoded and encoded payload limit:

```text
10 MiB
```

Per-turn limits are 8 images and 20 MiB decoded plus the corresponding base64 budget. Retained session context is limited to 32 images and 64 MiB decoded plus base64. A multi-image attachment is transactional: Ferrum queues none of the batch unless every image and the aggregate pass validation.

## CLI

Attach one image:

```bash
ferrum --image ./screenshot.png -p "describe this image"
```

Attach multiple images:

```bash
ferrum --image ./before.png --image ./after.png -p "compare these"
```

Ferrum validates actual PNG, JPEG, or WebP decodability under decoder dimension and allocation limits; a matching filename or signature alone is insufficient. It also detects image paths or `data:image/...;base64,...` blocks pasted into the prompt and attaches them automatically.

## Interactive mode

Attach an image to the next message:

```text
/image ./screenshot.png
```

Attach the current clipboard image:

```text
/paste-image
```

Then send the prompt:

```text
describe this image
```

You can also paste one or more image file paths directly into the prompt. Ferrum removes detected image paths from the text prompt and attaches them to the message.

If your terminal or file manager pastes images as `data:image/...;base64,...`, Ferrum creates a temporary preview file on the fly and attaches the image.

If your terminal sends Ctrl+V as a key sequence instead of text, Ferrum attempts to read the clipboard image with `xclip` on X11 or `wl-paste` on Wayland, writes it to a private Ferrum temporary directory, and processes that generated path like a normal pasted image path. Clipboard helpers have a 5-second deadline and a 10 MiB output cap.

## Preview

Ferrum previews images when attaching them:

1. If `chafa` is installed, Ferrum renders a terminal preview.
2. On terminals with known pixel-graphics support, Ferrum asks `chafa` for a high-resolution preview first:
   - Kitty/Ghostty: Kitty graphics protocol
   - iTerm2: iTerm inline image protocol
   - foot/mlterm/WezTerm or sixel-marked terminals: sixel output
3. If high-resolution rendering is unavailable or fails, Ferrum falls back to `chafa` symbol rendering.
4. Ferrum previews a private copy of already validated bytes rather than reopening the original path. `chafa` has a 10-second deadline and a 2 MiB output cap; temporary preview files are deleted afterwards.
5. If preview rendering is unavailable, Ferrum prints fallback metadata:

```text
[image] ./screenshot.png (image/png, ~12345 bytes, sha256:abc123...)
```

Install `chafa` with your distro package manager if you want terminal previews.

## Provider support

Implemented mappings:

- OpenAI-compatible Chat Completions multimodal content
- OpenAI Codex Responses multimodal input

Provider/model support varies. Some OpenAI-compatible endpoints may reject images even if text chat works.

## Sessions

Images are currently stored inline in JSONL session messages as base64 content. This is simple and portable, but large images can make sessions grow quickly.

Future improvement: session asset directories with path/hash references.
