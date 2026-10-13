---
id: art_00000000-0000-4000-8000-000000000005
kind: prd
version: 1
title: Demo PRD — Demonstrate the Relay workflow end to end (fake provider)
status: draft
createdAt: 2026-10-13T02:46:44.000Z
updatedAt: 2026-10-13T02:46:44.000Z
---

# Demo PRD — Demonstrate the Relay workflow end to end (fake provider)

## Summary

Demo requirements derived from the goal "Demonstrate the Relay workflow end to end (fake provider)" for pipeline demonstration purposes.

## Requirements

### REQ-1

The workflow shall produce one artifact per mode, versioned as Markdown.

_Rationale:_ Demonstrates artifact creation and approval checkpoints.

### REQ-2

The workflow shall pause before any delivery action.

_Rationale:_ Demonstrates the delivery hold policy.

## Exclusions

- Real model reasoning.
- Changes to user files.

## Acceptance criteria

### AC-1
- **Given:** the demo workflow has run through verification
- **When:** artifact storage is inspected
- **Then:** a Markdown artifact exists for every workflow mode

### AC-2
- **Given:** the delivery report exists
- **When:** the workflow attempts to finish
- **Then:** it stops and requires explicit user authorization for delivery actions
