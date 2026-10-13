---
id: art_00000000-0000-4000-8000-000000000002
kind: delivery-report
version: 1
title: Demo delivery report — Demonstrate the Relay workflow end to end (fake
  provider)
status: draft
createdAt: 2026-10-13T02:46:41.000Z
updatedAt: 2026-10-13T02:46:41.000Z
---

# Demo delivery report — Demonstrate the Relay workflow end to end (fake provider)

## Summary

Demo delivery trace for "Demonstrate the Relay workflow end to end (fake provider)" linking requirements to evidence; delivery actions are held.

## Trace

### REQ-1

The workflow shall produce one artifact per mode, versioned as Markdown.

- **Design decisions:**
  - Artifacts are rendered deterministically and stored under .relay/artifacts (demo).

- **Changes:** none

- **Evidence:**
  - demo run — artifacts exist for every mode

### REQ-2

The workflow shall pause before any delivery action.

- **Design decisions:**
  - Delivery always ends in a policy pause requiring user authorization (demo).

- **Changes:** none

- **Evidence:**
  - demo run — workflow paused with the delivery hold

## Next steps

- Review the demo artifacts.
- Delete the demo workspace when finished.

## Held actions

- git commit
- git push
- pull request creation
- deployment
- publishing
