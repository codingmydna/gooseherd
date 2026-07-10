## Summary
<!-- Describe your change. For agent-catalog PRs, name the agent and what serves it. -->

### Checklist

- [ ] `cargo fmt --check` and `cargo test -p goose-cli --lib` pass
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean

#### Adding an agent? (one-file PRs welcome — see [ADAPTERS.md](../ADAPTERS.md))

- [ ] `adapters/<name>.yaml` added with a working `install` one-liner
- [ ] Evidence pasted below: a ledger line or `/orch` / `/arena` output showing
      the agent completing a task

### Testing
<!-- How has this change been tested? Unit/integration tests? Manual testing? -->

### Related Issues
Relates to #ISSUE_ID  
Discussion: LINK (if any)

### Screenshots/Demos (for UX changes)
Before:  

After:   
