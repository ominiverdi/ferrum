# Images

Ferrum supports local image attachments for providers that accept multimodal input.

Status: early feature.

## Supported input formats

- PNG
- JPEG
- WebP

Maximum image size:

```text
10 MB
```

## CLI

Attach one image:

```bash
ferrum --image ./screenshot.png -p "describe this image"
```

Attach multiple images:

```bash
ferrum --image ./before.png --image ./after.png -p "compare these"
```

Ferrum also detects image paths or `data:image/...;base64,...` blocks pasted into the prompt and attaches them automatically.

## Interactive mode

Attach an image to the next message:

```text
/image ./screenshot.png
```

Then send the prompt:

```text
describe this image
```

You can also paste one or more image file paths directly into the prompt. Ferrum removes detected image paths from the text prompt and attaches them to the message.

If your terminal or file manager pastes images as `data:image/...;base64,...`, Ferrum creates a temporary preview file on the fly and attaches the image.

Raw clipboard pixel data cannot be read from a plain terminal unless the terminal converts it to a path or data URI.

## Preview

Ferrum previews images when attaching them:

1. If `chafa` is installed, Ferrum renders a terminal preview.
2. For pasted data URIs, Ferrum creates a temporary image file for the preview and deletes it afterwards.
3. If preview rendering is unavailable, Ferrum prints fallback metadata:

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
