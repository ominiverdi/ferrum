# Resource and terminal boundaries

This document defines Ferrum's resource-boundary contract for Codeberg issue #24.
Ferrum is host policy, not a sandbox: allowed tools, trusted local executables, decoders,
and concurrent host processes retain the authority of the Ferrum user.

## Threat model

Untrusted data includes provider text and errors, MCP data, tool output, repository file
contents and names, session metadata, image bytes, clipboard-helper output, and project
context files. It may be malformed, enormous, slow, raceable, or contain terminal control
sequences.

Ferrum must prevent that data from:

- emitting terminal control protocols such as CSI, OSC, DCS, APC, PM, SOS, C1 controls,
  BEL, or title terminators;
- causing an allocation proportional to an unbounded line, file, directory, process output,
  encoded image, or decoded image;
- leaving a partially accepted image batch or a partially replaced configuration/source file;
- making a native search ignore cancellation or its execution deadline;
- making a validated image path get reparsed by a preview helper after a path swap; or
- escaping a search root through a fallback-grep file symlink.

Ferrum does not promise process isolation, protection from a compromised `rg`, `chafa`,
image-decoder, kernel, or filesystem, or race-free behavior against an adversary controlling
ancestor directories. Writable-root policy remains the authority boundary for native
mutation. Atomic replacement protects the target pathname and detects target identity
changes; it is not a directory sandbox.

## Contract and limits

- Untrusted terminal text is sanitized at render boundaries. Printable Unicode, newline,
  carriage return, and tab remain; terminal control strings and other controls are removed.
  Streaming provider output uses a stateful sanitizer so split escape sequences cannot pass.
  Ferrum's own ANSI styling and explicitly selected `chafa` preview protocol output are
  emitted after this boundary.
- Terminal titles contain only sanitized printable text and are limited to 200 characters.
- Project context reads at most the remaining 128 KiB aggregate budget. More-specific
  `AGENTS.md` files retain priority.
- A single encoded or decoded image remains limited to 10 MiB. Images must be genuinely
  decodable as PNG, JPEG, or WebP under decoder dimension and allocation limits.
- A turn accepts at most 8 images and 20 MiB decoded / corresponding base64 bytes. A
  retained session accepts at most 32 images and 64 MiB decoded / corresponding base64
  bytes. A multi-image attachment commits only after every image and the aggregate pass.
- Clipboard helpers have a 5-second deadline and a 10 MiB + 1 output cap. Preview helpers
  consume a private copy of validated bytes, have a 10-second deadline, and have a 2 MiB
  output cap.
- Native `read` consumes lines incrementally and retains at most 50 KiB plus one bounded
  line buffer. Leading blank lines count toward the line limit.
- Native `grep` has a true global match limit, a 50 KiB output budget, bounded event/line
  buffers, cancellation, and a 10-second deadline. Its fallback streams files and rejects
  file symlinks and canonical escapes.
- Native `find` checks cancellation and a 10-second deadline during traversal.
- Native `ls` scans the directory but retains only the lexicographically smallest requested
  entries, bounding memory by the requested limit.
- Native write/edit and palette application use a sibling temporary file, sync it, verify
  that the destination identity is unchanged, rename atomically, and sync the parent.
  Symlink destinations are rejected. Existing permissions and ownership are preserved.
- Palette seeding considers each built-in independently and never replaces an existing
  palette.
- Automatic color selection is evaluated for the actual output stream.

## Regression matrix

| Boundary | Required regression |
| --- | --- |
| Terminal text | OSC, CSI, C1 OSC/ST, BEL, DCS, split streaming sequences, and embedded titles do not survive |
| Terminal title | C0/C1 controls and multiline title data cannot terminate or inject a title |
| Context | Invalid/huge data beyond the remaining budget is not read; specific context remains preferred |
| Image validation | Signature-only junk and oversized decoded dimensions are rejected; valid PNG/JPEG/WebP pass |
| Image aggregate | Count and byte limits reject without changing `pending_images`; valid batches commit together |
| Clipboard/preview | Hung and oversized helpers are killed/rejected; preview uses private validated bytes |
| Write/edit | Original survives staging or target-identity failure; target swaps are detected; mode, ownership, and content are preserved on success |
| Read | Huge line stays bounded; leading blank lines are represented and counted |
| Ripgrep | Limit is global across files; output/event buffers are bounded; cancellation terminates the child |
| Fallback grep | Huge lines/files stay bounded; context works; file-symlink escape is rejected |
| Find | Cancellation and deadline stop sparse traversal |
| Ls | Huge directory memory is bounded by the requested result count and output remains sorted |
| Palettes | Missing built-ins are repaired individually; existing custom files remain untouched; apply is atomic |
| Color routing | Redirected stdout does not inherit stderr TTY color state |

## Finding disposition

Issue #24 covers H08, H38-H48, H52-H56, M18-M23, and M55. H40 was completed
by issue #20's writable-root policy. Cancellation batching and descendant containment
remain in issue #25; this issue only adds cancellation and deadlines to the native search
operations named above.
