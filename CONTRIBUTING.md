# Contributing to gooseherd

gooseherd is a young, CLI-first fork of [goose](https://github.com/aaif-goose/goose).
Contributions are welcome — especially new agent adapters and improvements to the
orchestration loop.

## Build, test, lint

Plain cargo, no hermit:

```bash
cargo build                                                        # or: just build
cargo build --release -p goose-cli                                 # or: just release
cargo test                                                         # or: just test
cargo fmt                                                          # or: just fmt
cargo clippy --workspace --all-targets -- -D warnings # or: just lint
```

`just install` builds the release binary and copies it to `~/.local/bin/goose`.
Run `just` with no arguments to list all tasks.

## Adapters welcome

gooseherd drives any ACP-speaking CLI. Adding one is usually a single config
entry under `GOOSE_ACP_AGENTS` (see the README) — no Rust required. If you have
an agent worth bundling as a built-in preset, open a PR or an issue; the adapters
catalog is a first-class contribution surface.

## Before you open a PR

- `just lint` and `just test` pass.
- Keep changes focused and small, and describe how you verified them.
- Conventional-commit-style PR titles are appreciated.
