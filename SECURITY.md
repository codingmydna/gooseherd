# Security

## Reporting a vulnerability

Please report security vulnerabilities privately through GitHub Security
Advisories: https://github.com/codingmydna/gooseherd/security/advisories/new.
Do not open a public issue for a security problem.

## The orchestration permission model

gooseherd runs cheaper models under a frontier model's plan and does not trust
any single agent with unrestricted access. Planner and reviewer roles run
read-only: permission requests are judged by ACP tool kind, so reads, searches,
and read-only subagent exploration are approved while edits, deletes, and moves
are rejected (the vendor CLI's own restrictions — Claude Code plan mode, Codex
read-only sandbox — remain a second barrier). The implementer runs with write
access confined to its own git worktree, and with
`GOOSE_ORCH_IMPLEMENT_POLICY: allowlist` it is further restricted to an approved
command list. Mechanical gates run before the reviewer sees any diff. As with
any developer agent, review generated code and run untrusted tasks in an
isolated environment.
