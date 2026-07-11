# Context accounting and compaction boundaries

This document records the current Ferrum context-accounting discrepancy and the proposed long-lived fix.

## Observed problem

A session can show contradictory state after manual compaction:

```text
[session] context 92% used (237351/256000 estimated tokens); auto-compact will run at 95%

ferrum> /compact
conversation compacted: 65338 -> 23971 estimated tokens

ferrum> /session
context_tokens: 237351
context_source: usage+estimate
context_usage_percent: 92
```

The compaction result is based on Ferrum's local estimate of the compacted in-memory message set. The later `/session` value can still use provider-reported usage from a retained assistant message that was produced before compaction. That usage reflected the old large context, so it is stale after compaction.

## Current Ferrum behavior

Ferrum stores provider usage on assistant messages. `AgentState::stats()` prefers `context_tokens_from_usage()` when available:

```text
latest assistant usage + local estimate for messages after that assistant message
```

That is useful during normal turns because provider usage is usually more accurate than character-count estimation. The problem is that compaction changes the effective context boundary. A retained assistant message may keep usage from before the boundary, even though the old context has been summarized away.

So provider usage is accurate for billing/accounting history, but not always valid for current context pressure.

## Prior art: Pi

The installed Pi source has explicit guards for this.

Relevant files inspected:

- `dist/core/compaction/compaction.js`
- `dist/core/agent-session.js`
- `dist/modes/interactive/components/footer.js`
- `dist/core/session-manager.js`

Pi has the same basic estimate model:

```text
last assistant usage + estimated trailing messages
```

But it treats compaction as a boundary. Comments in `dist/core/agent-session.js` state that after compaction, the last assistant usage reflects pre-compaction context size and can only be trusted if it came from an assistant response after the latest compaction.

Pi's UI also treats context usage as unknown immediately after compaction until a fresh post-compaction LLM response exists. `dist/modes/interactive/components/footer.js` says:

```text
After compaction, tokens are unknown until the next LLM response.
```

Pi's compaction entries store more boundary metadata than Ferrum currently does, including:

- `summary`
- `firstKeptEntryId`
- `tokensBefore`
- `details`
- `fromHook`

## Desired Ferrum invariant

Provider usage must not cross a compaction boundary.

More precisely:

- Provider usage on an assistant response is valid for current-context accounting only if that assistant response is after the latest compaction boundary.
- Provider usage before the latest compaction remains valid for historical `/usage` totals, but not for `/session` context pressure.
- Local estimates remain acceptable for current context pressure when no post-compaction provider usage exists.

## Proposed behavior

Implemented in Ferrum after this note was written: `/session` no longer reports stale pre-compaction usage. After compaction, current-context accounting falls back to a local estimate until a fresh post-compaction assistant response exists.

Expected shape immediately after compaction:

```text
context_tokens: 23971
context_source: estimate_after_compaction
context_usage_percent: 9
```

This differs slightly from Pi, which displays unknown context usage until the next LLM response. Ferrum already uses local estimates for pressure warnings and manual session status, so reporting an estimate is more useful and consistent than showing unknown.

After the next assistant response, Ferrum returns to:

```text
context_source: usage+estimate
```

because the new provider usage was produced after the compaction boundary.

## Implementation direction

### Step 1: boundary-aware stats

Implemented: current context accounting ignores usage from assistant messages before the latest compaction boundary. Compaction clears usage on retained assistant messages and re-appends retained messages after the persisted compaction entry, so in-memory and resumed sessions share the same boundary. Current-request preflight uses the larger of this provider-informed value and the complete local request estimate, including pending messages and tool schemas.

Original implementation options considered:

1. In-memory tactical fix:
   - When compaction builds retained messages, clear `usage` on retained assistant messages.
   - Then `stats()` naturally falls back to local estimate until a fresh assistant response arrives.

2. Boundary-aware long-lived fix:
   - Track or infer the latest compaction boundary.
   - Make `context_tokens_from_usage()` consider only assistant usage after that boundary.
   - Preserve usage fields on retained messages for historical/cost inspection.

The second shape is cleaner long-term. The first shape is simpler but loses in-memory historical usage details until reload.

### Step 2: source labels

Extend context source labels:

- `usage+estimate`: post-boundary provider usage plus trailing local estimate
- `estimate`: no provider usage available
- `estimate_after_compaction`: latest available provider usage is before the latest compaction boundary, so current context pressure uses local estimate

### Step 3: tests

Add regression tests for:

1. A session with high assistant usage compacts and then `/session` no longer reports the stale high value.
2. Context source becomes `estimate_after_compaction` after compaction if no post-compaction assistant usage exists.
3. A new assistant message with fresh usage after compaction restores `usage+estimate`.
4. Historical usage totals remain independent from current context accounting.

### Step 4: persisted metadata

Later, extend compaction entries with optional metadata while keeping backward compatibility:

- `before_tokens_estimate`
- `after_tokens_estimate`
- `archived_message_count`
- `first_kept_message_id` or another boundary marker

Older sessions without these fields should continue to load. For old sessions, Ferrum can treat the compaction entry itself as the boundary and fall back to local estimates until a later assistant usage appears.

## Non-goals

- Do not change `/usage` cost/accounting totals as part of this fix.
- Do not rewrite the whole session format.
- Do not make compaction depend on provider-specific token counting APIs.
- Do not discard durable assistant usage records from JSONL just to fix current context pressure.

## Recommendation

Implement boundary-aware current-context accounting first. Keep provider usage for historical accounting, but use it for `/session` and compaction pressure only when it is post-compaction. Display `estimate_after_compaction` between compaction and the next assistant response.
