# Relay repository instructions

## Product direction

Relay is a local-first, model-agnostic CLI that carries product intent through discovery, specification, technical design, implementation, verification, and delivery. Models are replaceable workers; Relay owns workflow state, artifacts, permissions, and transition policy.

## Engineering rules

- Use TypeScript on Node.js 22 or later, ESM, strict compiler settings, and npm.
- Keep the workflow core independent of provider SDK types. Translate provider responses into Relay-owned request and event types at adapter boundaries.
- Model workflow changes as explicit, replayable events. Persist durable project state locally and keep PRDs, TRDs, plans, and reports as readable Markdown artifacts.
- Let language models propose intents and transitions; deterministic application code validates and applies them.
- Treat user files and dirty Git worktrees as owned by the user. Never overwrite, discard, commit, or push changes unless explicitly requested.
- Require user intervention for consequential product choices, changes to approved artifacts, destructive operations, deployment, publishing, and delivery. Continue routine reversible work automatically.
- Prefer small modules with single ownership. Coordinate before changing shared contracts such as core events, provider interfaces, database schemas, and configuration schemas.
- Validate all model-produced structured data and tool arguments at runtime with Zod.
- Use `spawn`-style process execution with argument arrays. Do not interpolate untrusted input into shell command strings.
- Keep secrets in environment variables or OS-backed credential storage. Never persist API keys in project state or logs.
- Add focused tests for state transitions, policy gates, provider normalization, persistence replay, and tool safety. Avoid tests that merely mirror implementation details.

## Required checks

Run these before reporting implementation complete:

```bash
npm run typecheck
npm test
npm run build
```

Do not commit or push unless the user explicitly asks.
