---
id: art_00000000-0000-4000-8000-000000000003
kind: execution-plan
version: 1
title: Demo execution plan — Demonstrate the Relay workflow end to end (fake provider)
status: draft
createdAt: 2026-10-13T02:46:42.000Z
updatedAt: 2026-10-13T02:46:42.000Z
---

# Demo execution plan — Demonstrate the Relay workflow end to end (fake provider)

## Summary

Demo task graph for "Demonstrate the Relay workflow end to end (fake provider)": one task per downstream mode, no user-file changes.

## Tasks

### Task TASK-1 — Produce demo artifacts

Each mode emits its schema-valid demo draft for approval.

- **Dependencies:** none

- **Affected files:** none

- **Checks:**
  - `demo check: artifact files exist under .relay/artifacts`

- **Integration points:** none
