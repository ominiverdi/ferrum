# Background tasks design note

This is an exploratory design note. It is not an implementation plan yet.

Ferrum may eventually need a first-class way to run independent agentic work in the background. The motivating case is a long-running operation such as a model download, build, training job, sync, or backup where a normal foreground tool call is the wrong abstraction.

## Problem

Today Ferrum has two rough options:

- run a foreground `bash` command with a timeout
- ask the model to create detached `nohup` or system-level automation

Both are useful, but neither is a good long-term model-owned task abstraction.

Foreground commands block the turn and eventually time out. Detached shell scripts survive, but Ferrum does not own their state, logs, lifecycle, permissions, or event delivery. The model can inspect them later only by remembering ad-hoc paths and commands.

## Goal

A background task feature should let the model start bounded, inspectable, user-visible work that continues while Ferrum is idle.

The user should be able to say:

```text
Keep an eye on this download and tell me when it is complete.
```

The model should be able to start a task without the user learning a new slash command. When the task reports something important, Ferrum should surface that event clearly and add it to the session context so both the user and the model can see it.

## Non-goals for an initial version

- Hidden autonomous mutation.
- Surprise model calls while the user is away.
- Unbounded shell loops with no audit trail.
- A full job scheduler.
- A replacement for systemd, cron, or CI.

A first version should observe and report. It should not independently spend tokens or mutate files unless that behavior is explicitly authorized.

## Possible levels

### Level 1: passive monitors

A monitor runs a command periodically and emits events when something changes, fails, matches a stop condition, or completes.

No model calls happen in the background.

### Level 2: model-assisted background tasks

A task may ask a model to summarize or classify an event, but does not execute follow-up tools without policy.

### Level 3: autonomous bounded tasks

A task has an explicit goal, allowed tools, budget, max runtime, max model calls, and permitted paths. It can act independently within those limits and emits an audit trail.

### Level 4: task supervisor

Multiple tasks with dependencies, notifications, resumption across Ferrum restarts, and richer task management.

## Preferred UX direction

This should be tool-first, not slash-command-first.

Potential tools:

- `background_task_start`
- `background_task_status`
- `background_task_stop`

For passive monitors, the start payload might look like:

```json
{
  "name": "diffusiongemma-download",
  "goal": "Watch download until all 11 shards are present",
  "command": "check command...",
  "interval_seconds": 300,
  "stop_when_contains": "complete_shards=11/11",
  "max_runs": 200,
  "timeout_seconds_per_run": 60
}
```

The model chooses when to start, inspect, and stop a task. The user does not need to manually invoke task commands in normal use.

Slash commands may still be useful for emergency control and visibility, for example:

```text
/tasks
/tasks stop <name>
/tasks show <name>
```

But they should not be the primary UX.

## Event delivery

The key idea is a task event inbox.

A background worker should not write directly into the active session JSONL. Instead, it should write durable events into a queue owned by Ferrum, for example:

```text
~/.local/share/ferrum/task-events.jsonl
```

The interactive Ferrum process drains relevant events for the active session and appends a synthetic system-style message into the session.

Example visible output while Ferrum is idle:

```text
------
[background:diffusiongemma-download]
complete_shards=11/11
download process ended
------
ferrum>
```

On the next user turn, the model sees the event in context and can decide what to do next.

Ferrum should not automatically call the model just because a background event arrived. That avoids surprise token spend and surprise actions.

## State model

A task record could be stored as JSON:

```json
{
  "id": "download-diffusiongemma",
  "kind": "monitor",
  "goal": "Watch download until all 11 shards are present",
  "session_path": "...",
  "created_by": "model",
  "status": "running",
  "command": "...",
  "cwd": "...",
  "schedule": {
    "every_seconds": 300
  },
  "limits": {
    "max_runs": 200,
    "max_runtime_seconds": 86400,
    "max_output_bytes_per_event": 8000,
    "timeout_seconds_per_run": 60
  },
  "permissions": {
    "tools": ["bash", "read", "find"],
    "write": false,
    "network": false
  },
  "stop_when": {
    "contains": "complete_shards=11/11"
  }
}
```

Task files could live under:

```text
~/.local/share/ferrum/tasks/<id>.json
~/.local/share/ferrum/tasks/<id>.log
```

## Safety requirements

A proper implementation should include:

- clear user-visible task start messages
- durable task registry
- durable event queue
- explicit stop controls
- bounded output per event
- max runtime and max run limits
- permission model for tools and file mutation
- audit log of commands, outputs, model decisions, and state transitions
- recovery behavior after Ferrum restarts
- no direct session writes from worker processes

## Open questions

- Should passive monitors be implemented first under a narrower name, or should the first user-facing API already say `background_task`?
- Should event delivery refresh an idle prompt immediately, or only drain before/after turns at first?
- How should the user approve autonomous mutation for Level 3 tasks?
- Should background model calls be disabled by default, enabled per task, or never part of this feature?
- How should task permissions interact with project `AGENTS.md` and Ferrum tool allow/deny config?
- How should task events be compacted or summarized over long sessions?

## Recommended path

Start with Level 1 passive monitors only:

1. Add task registry and event queue.
2. Add model-facing tools to start, inspect, and stop a passive monitor.
3. Add event draining before and after interactive turns.
4. Append drained events to the session as visible synthetic system messages.
5. Later consider idle prompt refresh and desktop notifications.

Do not start with autonomous mutation. Build the durable event and audit substrate first.
