# Adding your agent to gooseherd

Any CLI that speaks [ACP](https://agentclientprotocol.com) can be a gooseherd
planner, implementer, or reviewer. If yours isn't in the catalog yet, adding it
is a one-file pull request.

## Try it locally first (no code required)

Add an entry to `~/.config/goose/config.yaml`:

```yaml
GOOSE_ACP_AGENTS:
  myagent: myagent --acp        # whatever starts your agent in ACP mode
```

That registers a `myagent-acp` provider you can assign to any role:

```
/roles implementer=myagent-acp/<model>
```

If the agent needs environment variables (API base URLs, tokens), use the map
form — `${VAR}` is resolved from your shell or the goose secret store at start:

```yaml
GOOSE_ACP_AGENTS:
  myagent:
    command: some-acp-adapter
    env:
      MYAGENT_BASE_URL: https://api.example.com
      MYAGENT_API_KEY: ${MYAGENT_API_KEY}
```

Run `goose herd` to confirm the adapter resolves, then run one small `/orch`
task with your agent as the implementer.

## Ship it as a catalog entry

Once it works, copy an existing file in [`adapters/`](adapters/) and fill in
yours:

```yaml
name: myagent
description: One line on what serves it and why you'd pick it.
command: myagent --acp          # or the map form above
install: npm install -g myagent
auth: Run `myagent login` once; gooseherd reuses the session.
models:
  - example-model-id
status: community               # verified = a maintainer reproduced a full /orch run
homepage: https://example.com
```

That single YAML file is the whole contribution. `goose herd` picks it up and
shows its install state, and `goose herd add myagent` writes the config for
every user after you. Verify the file parses with:

```sh
cargo test -p goose-cli adapters
```

Then open a PR containing:

1. `adapters/<name>.yaml`
2. In the PR description: the `/orch` or `/arena` output showing your agent
   completing a task (the ledger line is enough).

`status: verified` is granted when a maintainer reproduces a full
plan→implement→review cycle with your agent; until then it ships as
`community` and still works for everyone.

## What makes a good catalog entry

- `install` must be a single copy-pasteable command.
- `auth` should say where the session/key lives, not just "log in".
- Prefer the vendor's own CLI or an official adapter over third-party proxies
  when both exist — subscriptions are governed by vendor terms.
- If the agent has a free tier or a notably cheap model, say so in
  `description` — cheap implementers are what most people come here for.
