# voff-provider

Provider-neutral interfaces and integrations for connecting Voff workflows to
coding-agent providers and local command-line agents.

This crate is part of [Voff](https://github.com/orhun/voff), a terminal-first
workflow builder for coding agents.

Local Codex and Claude providers keep one child process per agent session.
The provider serializes turns through that process, streams each turn through
the normal provider event interface, and resumes the persisted vendor session
when Voff starts again. A failed or cancelled turn retires the process so a
later attempt can safely reopen the saved session. These child processes run
from Voff's private session directory; workspace reads, patches, and commands
continue to pass through Voff's scoped, approval-gated tools.
