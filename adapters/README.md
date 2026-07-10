# ACP agent adapters

Each YAML file in this directory adds one ACP agent to the catalog embedded in
`goose`. Adding an agent should be a one-file pull request.

| Field | Description |
| --- | --- |
| `name` | Unique catalog name; must match the YAML file stem |
| `description` | One-line agent description |
| `command` | A command string or the same `{ command, env, env_remove }` map accepted by `GOOSE_ACP_AGENTS` |
| `install` | One-line installation command |
| `auth` | One-line login or API-key guidance |
| `models` | Example model IDs for role assignment |
| `status` | `verified` or `community` |
| `homepage` | Agent homepage URL |

To contribute an adapter, copy an existing YAML file, keep every value grounded
in the agent's official documentation, and run:

```sh
cargo test -p goose-cli adapters
```

See [`../ADAPTERS.md`](../ADAPTERS.md) for the full walkthrough.
