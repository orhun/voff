# Product description

## Vision

voff is an open-source, terminal-first orchestration system for running local
and cloud AI models as a role-based software team. Users can assign any model
to any role, run a single agent, or build custom formations without coupling
the workflow to a specific provider.

## Problem

Multi-agent software development scatters tasks, context, file changes,
permissions, and results across separate tools. Controlled delegation,
integration, and human supervision are difficult when each agent operates in
its own silo.

voff brings those concerns into one workspace:

- provider and model selection;
- custom roles, prompts, tools, budgets, and permissions;
- explicit task decomposition and delegation;
- parallel and dependency-ordered execution;
- shared workspace coordination and conflict handling;
- human approval for risky actions;
- live terminal views for tasks, events, costs, artifacts, and diffs.

## Intended users

- Developers who use multiple AI coding tools together.
- Solo developers who want a supervised virtual software team.
- Open-source communities experimenting with provider-neutral workflows.
- Teams that need repeatable and auditable AI-assisted development.

## Core experience

1. Open a workspace and choose a ready-made or custom formation.
2. Configure each agent's provider, model, role, tools, budget, and permissions.
3. Give an objective to the entry agent or a selected specialist.
4. Review the resulting task graph, live output, usage, and file changes.
5. Approve or deny risky commands, delegations, and conflicting changes.
6. Close work only when results include the required evidence and checks.

## Product boundaries

voff is not a model-training platform, a general-purpose IDE, or a hosted CI
service. Its current scope is a local terminal interface, durable task
orchestration, controlled workspace tools, and provider adapters. Remote
workers, a web UI, a marketplace, and distributed collaboration are future
possibilities rather than current commitments.

## Success criteria

- A new user can configure and run a first team within ten minutes.
- Changing providers does not require changes to orchestration logic.
- Every task has a traceable owner, state, input, output, and tool history.
- Agents cannot execute risky actions without policy authorization.
- Interrupted sessions can be reopened without inventing successful results.
- Single-agent use remains simple while sharing the same safety model.
