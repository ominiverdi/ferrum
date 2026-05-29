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

Project locations are discovered from the current directory upward to the git repository root. Project skills override global skills with the same name.

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

Full skill instructions are loaded on demand with slash commands or by the model reading the skill file.

## Commands

List skills:

```text
/skills
```

Load a skill:

```text
/skill pdf-tools
/skill pdf-tools extract file.pdf
/skill:pdf-tools extract file.pdf
```

Loading a skill appends the full skill file as a system message and persists it in the JSONL session.

## Security

Skills are instructions, not trusted code. Ferrum does not automatically run setup scripts from skills. Any scripts are executed only if the model calls tools and the current tool policy permits it.
