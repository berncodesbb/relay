# Examples

## `relay.config.yaml`

A complete, commented Relay project-layer configuration demonstrating:

- named **provider connections** on the single live external protocol: the
  canonical `anthropic` connection, two more Anthropic Messages-compatible
  endpoints (`gateway` and `local` — capability and endpoint differences are
  pure configuration), plus the offline `fake` connection;
- an explicit **capability override** (`structuredOutput: json-object`)
  winning over the anthropic adapter defaults;
- the five built-in **model roles** redefined against live connections, plus
  a custom `security-review` role;
- role **fallbacks** and per-role **limits**;
- the full **routing** table (the seven workflow modes plus `intent`), with a
  custom role owning `verification`;
- budgets, autonomy, and tool settings.

### Using it

```bash
# 1. Inspect the effective result of this file (no state is created):
relay --workspace examples config show

# 2. Or copy it into your own project as the project layer:
cp examples/relay.config.yaml ~/your-project/relay.config.yaml

# 3. Export the credentials your connections name (never put values in the
#    file):
export ANTHROPIC_API_KEY=...
export GATEWAY_API_KEY=...
export LOCAL_GATEWAY_KEY=...

# 4. Verify and run:
relay config path        # which layers applied
relay config show        # the merged effective configuration
relay run                # routes through the connections above
```

Without any configuration at all, Relay runs fully offline: the built-in
defaults route every role to the deterministic `fake` connection — no keys,
no network. See [docs/configuration.md](../docs/configuration.md) for every
field, the capability-profile system, layer merge rules, more examples
(user-layer overrides, legacy inline routes, offline pinning), and the
migration errors that removed `openai-chat` configurations now produce.
