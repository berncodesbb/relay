---
id: art_00000000-0000-4000-8000-000000000006
kind: trd
version: 1
title: Demo TRD — Demonstrate the Relay workflow end to end (fake provider)
status: draft
createdAt: 2026-10-13T02:46:45.000Z
updatedAt: 2026-10-13T02:46:45.000Z
---

# Demo TRD — Demonstrate the Relay workflow end to end (fake provider)

## Summary

Demo technical design for "Demonstrate the Relay workflow end to end (fake provider)": the pipeline itself is the architecture under demonstration.

## Repository findings

| Area | Finding |
| --- | --- |
| demo | Repository facts are not inspected by the demo script; this finding is a placeholder. |

## Architecture

Demo architecture: the workflow controller drives seven modes over an append-only event log; providers are replaceable workers.

## Interfaces

- **WorkflowController** — Drives mode steps and applies transition policy (demo description).

## Migrations

_(none recorded)_

## Risks

- Demo content may be mistaken for real analysis. — mitigation: Every demo artifact title and summary is labeled "demo".

## Rollout

Demo rollout: run the workflow with the fake provider in a disposable workspace.

## Rollback

Delete the demo workspace; nothing outside it is touched.
