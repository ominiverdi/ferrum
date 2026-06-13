# Provider secrets investigation

This document records a first investigation for issue #18: encrypted provider secrets.

## Goal

Ferrum currently relies on provider API keys coming from environment variables such as `OPENAI_API_KEY`, `MINIMAX_API_KEY`, and similar provider-specific env vars.

That works well for:

- CI
- scripts
- system services

But it is awkward for interactive usage such as:

- `/providers`
- `/provider <name>`
- `/models`

where users may want Ferrum to already know the provider secret without pre-exporting an environment variable in every shell.

## Current recommendation

No implementation yet.

Reason: secret storage has real security implications and should not be added casually. We should gather more feedback first and avoid shipping a weak or desktop-only design by accident.

## Constraints

Ferrum is used both:

- on desktops
- on headless servers

So a desktop-only keyring story is not enough.

## Existing behavior

Keep environment variables as a first-class mechanism.

Recommended precedence if a secret feature is ever implemented:

```text
explicit env var > stored provider secret > error
```

Reason:

- preserves current scripting/CI behavior
- avoids surprising overrides
- keeps non-interactive deployments simple

## Candidate crates reviewed

### 1. `secrecy`

Role:

- in-memory secret wrapper
- prevents accidental logging/debug exposure as much as possible
- zeroizes memory on drop

Assessment:

- very likely useful as a supporting crate regardless of storage backend
- small dependency footprint
- not a storage backend by itself

Takeaway:

- strong candidate as a supporting dependency
- not enough alone for issue #18

### 2. `keyring` / `keyring-core`

Role:

- desktop/keyring integration
- Secret Service on Linux
- native stores on other platforms

Assessment:

- strong candidate for a desktop backend
- actively maintained and serious project history
- current docs explicitly state application/library users should prefer the core API layer rather than depending on the top-level sample CLI crate directly
- does not solve the full server/headless story by itself

Takeaway:

- good candidate for a desktop backend
- not sufficient as the only solution if Ferrum is expected to run mostly on servers

### 3. `securestore`

Role:

- encrypted, file-backed secret storage
- CLI and Rust library
- designed for deployed applications with an encrypted store and a separate key file

Assessment:

- the most interesting candidate for a server/headless file backend
- much closer to Ferrum's server-first needs than desktop keyring storage
- appears serious and well documented
- but the workflow may be heavier than Ferrum users expect for storing one or two provider API keys
- key-file management and UX need careful evaluation

Takeaway:

- worth a deeper targeted evaluation if Ferrum decides to support a built-in file backend
- not an automatic yes without more design work

### 4. `secret-vault`

Role:

- memory-backed secret access abstraction with cloud integrations
- AWS/GCP/K8S/env/file sources
- optional memory encryption and refresh behavior

Assessment:

- broad, powerful, and serious project
- but much larger in scope than Ferrum's likely need
- dependency surface is heavy
- docs.rs documentation coverage is poor
- more suitable for application/cloud secret orchestration than local provider secret UX

Takeaway:

- likely too large and wrong-shaped for Ferrum's initial secret feature

## Why this is not implemented yet

Main reasons:

1. Desktop-only storage would not fit server-first Ferrum use.
2. File-backed encryption needs proper design, not ad-hoc crypto.
3. Secret UX must be clear for both interactive and unattended/service use.
4. We should avoid shipping a half-solution that later becomes hard to undo.

## Recommended architecture direction if revisited later

Use a layered design:

1. `secrecy` for in-memory handling
2. backend abstraction for storage
3. keep env vars first-class

Possible backend families:

```text
env
keyring
file
```

Potential interpretation:

- `env`: current behavior, always supported
- `keyring`: desktop convenience backend
- `file`: server/headless encrypted local backend

## Likely phased path if revisited

### Phase 1

Clarify UX and threat model.

Questions to answer:

- Is server/headless the main target?
- Should Ferrum support unattended decryption?
- How should key material for file-based secrets be unlocked?
- What is acceptable for local-at-rest protection?

### Phase 2

Adopt `secrecy` in provider key handling code paths.

This improves in-memory handling even before adding a new storage backend.

### Phase 3

Prototype one backend only.

Most likely one of:

- keyring backend for desktop
- file backend for server/headless

### Phase 4

Add CLI only after backend choice is stable.

Potential commands:

```bash
ferrum secret set <provider>
ferrum secret list
ferrum secret rm <provider>
```

## Recommendation

Do not implement provider secrets yet.

Current position:

- keep issue #18 open
- keep environment variables as the supported mechanism
- invite community suggestions, especially from users with server-first workflows
- revisit after more input and after a clearer backend choice

## References

Useful upstream references reviewed during this investigation:

- `secrecy`: https://docs.rs/crate/secrecy/latest
- `keyring`: https://docs.rs/crate/keyring/latest
- `securestore-rs`: https://github.com/neosmart/securestore-rs
- `secret-vault`: https://docs.rs/crate/secret-vault/latest
