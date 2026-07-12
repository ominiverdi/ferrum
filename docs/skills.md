# Skills

Ferrum supports Agent Skills-style instruction packages with progressive disclosure.

Status: early feature.

## Locations

Global:

```text
~/.config/ferrum/skills/
~/.agents/skills/
```

Project:

```text
.ferrum/skills/
.agents/skills/
```

Project locations are discovered from the current directory upward to the git repository root. Project skills override global skills with the same name. A restrictive `.ferrum/config.toml` may set `skills.inherit_global = false` and apply skill allow/deny filters; see [Configuration](config.md#project-local-restrictive-policy).

## Structure

Preferred structure:

```text
my-skill/
  SKILL.md
  scripts/
  references/
  assets/
```

Ferrum also discovers direct `.md` skill files in Ferrum-specific directories:

```text
~/.config/ferrum/skills/example.md
.ferrum/skills/example.md
```

Direct root `.md` files are ignored in `.agents/skills/` for compatibility with other harnesses.

## SKILL.md

```markdown
---
name: pdf-tools
description: Extracts text and tables from PDF files. Use when working with PDFs.
---

# PDF Tools

Instructions...
```

Required frontmatter:

- `name`
- `description`

Name rules:

- lowercase letters, numbers, hyphens
- 1-64 characters
- no leading/trailing hyphen
- no consecutive hyphens

## Runtime behavior

At startup, Ferrum discovers skills and adds only skill names, descriptions, paths, and dirs to the system prompt.

Slash-command invocation expands the full skill body into a Pi-style `<skill>` block and immediately runs a model turn with that expanded prompt. Frontmatter is stripped before expansion. Skill-relative files should be resolved relative to the skill directory shown in the block.

The model can also inspect skill files with tools when appropriate.

Discovery order is deterministic. Global roots are considered first, followed by project roots from the validated repository boundary toward the current directory; the last valid skill name wins, so nearer project definitions override broader and global definitions. A `.git` marker is accepted as a repository boundary only when it is a directory or a valid gitfile pointing at an existing git directory.

## Commands

List skills:

```text
/skills
```

Run a skill:

```text
/skill pdf-tools
/skill pdf-tools extract file.pdf
/skill:pdf-tools extract file.pdf
```

Running a skill sends the expanded skill prompt as the next user turn and persists the resulting conversation in the JSONL session.

## Security

Skills are instructions, not trusted code. Ferrum does not automatically run setup scripts from skills. Any scripts are executed only if the model calls tools and the current tool policy permits it.

Skill discovery canonicalizes every candidate and enforces trust-root containment at every safety tier. Repository-controlled skill links may not resolve outside their project skill root. Global skill links may leave their global root only with an explicit opt-in:

```toml
[skills]
allow_external_global_symlinks = true
```

The default is `false`. This option applies only to global roots; it never permits project skill links to escape. External global targets remain untrusted instructions and gain no additional tool authority. Discovery also bounds traversal depth, candidate count, frontmatter size, and skill-body reads; skips symlink cycles; and resolves duplicate names deterministically by documented root precedence.
