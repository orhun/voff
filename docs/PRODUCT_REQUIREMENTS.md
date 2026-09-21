# Product Requirements Document

## 1. Product goal

Enable a user to configure one or more AI agents, assign them software-development roles, execute a traceable task graph and supervise the entire workflow from a terminal UI.

## 2. Personas

- **Solo developer:** wants a virtual team but needs simple defaults.
- **Power user:** wants custom prompts, models, tools, budgets and workflows.
- **Maintainer:** adds providers and plugins without changing the orchestration core.
- **Security-conscious team:** needs approvals, audit records and strict workspace limits.

## 3. Core concepts

| Concept | Definition |
| --- | --- |
| Provider | API, CLI or local runtime that serves one or more models |
| Agent | Named runtime identity with role, prompt, provider/model and permissions |
| Formation | Reusable collection of agents and collaboration rules |
| Task | Persisted unit of work with owner, criteria, limits and lifecycle |
| Tool | Capability such as file read, patch, shell, search or test execution |
| Workspace | Project root and isolated execution context |
| Session | One resumable orchestration run |
| Artifact | File, diff, report, test result or structured output produced by a task |

## 4. Functional requirements

### FR-1 Provider and model registry

- List configured providers and available models.
- Support capability discovery and explicit capability overrides.
- MVP: mock provider plus at least two real adapters chosen during implementation.
- Adapters may use official HTTP APIs or installed CLIs, but must expose the same core contract.
- Missing credentials or unsupported capabilities must fail clearly.

### FR-2 Agent profiles

- Create, clone, edit, enable and disable agents.
- Configure name, role, system prompt, provider, model, tools, workspace scope, budget, delegation rights and concurrency.
- Load prompts from inline text or workspace-relative files.
- Validate duplicate names, missing providers and excessive permissions.

### FR-3 Team formations

- Provide single-agent, lead-and-specialists and review-gated templates.
- Allow any agent to fill any role.
- Allow multiple roles to map to one agent.
- Define who may delegate to whom and whether user approval is required.
- Export/import formations without embedded secrets.

### FR-4 Task graph

- Create root and child tasks with dependencies.
- Show queued, ready, running, waiting approval, blocked, completed, failed and cancelled states.
- Reject graph cycles and illegal state transitions.
- Enforce maximum delegation depth, children, concurrency, time, tokens and cost.
- Persist task input, structured output, evidence and relationship metadata.

### FR-5 Agent-to-agent delegation

- An agent can emit a structured delegation proposal.
- The engine validates target, authority, budget, depth and dependency rules.
- According to policy, dispatch automatically or request human approval.
- The parent receives a normalized child result, not unrestricted hidden state.
- Cancellation can propagate according to an explicit policy.

### FR-6 Tools and execution

- MVP tools: scoped file read, search, patch proposal, shell command proposal and test result capture.
- Each tool call has typed input, authorization result, lifecycle and output limits.
- Shell and file writes execute only through the controlled executor.
- Risky actions require preview and approval.
- Output truncation must be explicit and raw output may be stored as an artifact.

### FR-7 Ratatui interface

- Dashboard: sessions, tasks, agents and aggregate usage.
- Workspace: task tree, selected task conversation/events, agents and status bar.
- Inspector: prompt, tool call, diff, artifact, error and usage details.
- Command palette for common operations.
- Keyboard navigation, help overlay, pause/cancel and approval dialog.
- Stream output without blocking rendering.
- Show cross-role relationships as directional specialist-to-Lead-to-specialist
  routes; never imply direct peer dispatch when policy requires mediation.

### FR-8 Persistence and recovery

- Use SQLite for metadata/events and files for large artifacts.
- Resume a session after graceful or unexpected process termination.
- Running work recovered after a crash becomes interrupted/unknown until reconciled.
- Version stored schemas and configuration.
- Provide export with optional redaction.

### FR-9 Observability and budgets

- Record canonical events with timestamps and correlation IDs.
- Track token usage, cost, latency, retries and tool duration where available.
- Enforce session, agent and task budgets.
- Display unknown or estimated cost honestly.
- Support redacted diagnostic bundles.

### FR-10 Extensibility

- Add providers through a documented adapter interface.
- Add tools through a capability and permission contract.
- Keep a versioned config schema.
- MVP uses compile-time adapters; dynamic third-party plugins are post-MVP.

## 5. Primary user stories and acceptance criteria

### US-1 Create a custom AI team

As a user, I can assign arbitrary providers/models to arbitrary roles.

Acceptance:

- No role is tied to a provider in source code.
- Guided setup requires exactly one entry Team Lead and keeps specialists from
  acquiring peer-dispatch authority.
- After setup, the user can add a safe preset specialist or change an idle
  specialist's availability without changing Team Lead authority.
- Invalid model/provider combinations are reported before session start.
- The formation can be saved and loaded with no secrets included.

### US-2 Delegate backend work

As a Team Lead agent, I can delegate a bounded backend task to another agent.

Acceptance:

- The child task has parent, owner, objective and acceptance criteria.
- Policy is checked before dispatch.
- Root work and delegation decisions originate from the configured Team Lead;
  specialists return results to that coordinator.
- A configured run assigns one non-overlapping child task to every enabled,
  allowed specialist and waits for all assigned results.
- The UI shows both tasks and their relationship.
- Child failure does not appear as successful parent completion.

### US-3 Use only one agent

As a solo user, I can run the whole workflow with one selected agent.

Acceptance:

- A single-agent formation requires no dummy roles.
- Delegation UI is hidden or simplified when disabled.
- Tasks, tools, approval and persistence work identically.

### US-4 Approve a risky command

As a user, I can inspect and approve or deny a risky tool request.

Acceptance:

- The dialog shows agent, task, command/action, working directory, risk reason and scope.
- Denial is returned to the agent as a structured result.
- Approval and denial are audited.
- “Approve once” cannot silently become a permanent rule.

### US-5 Recover a session

As a user, I can reopen a previous session after a crash.

Acceptance:

- Completed task history remains readable.
- Unconfirmed active operations are not assumed successful.
- The user can retry, cancel or reconcile interrupted tasks.

## 6. Non-functional requirements

- **Responsiveness:** UI input/rendering must not wait on network or processes.
- **Reliability:** state mutations are transactional where consistency requires it.
- **Portability:** target Linux and macOS first; Windows support follows executor validation.
- **Accessibility:** status uses text/symbols as well as color; keys are discoverable.
- **Performance:** support at least 32 configured agents and 1000 tasks/session; active concurrency remains bounded.
- **Privacy:** no telemetry by default. Any future telemetry is opt-in and documented.
- **Compatibility:** config and database migrations are explicit and versioned.

## 7. MVP definition

MVP includes:

- Rust workspace and Ratatui TUI
- Mock adapter and two provider adapters
- Agent/formation configuration
- Persistent task tree and structured delegation
- Controlled file/shell tools with approvals
- Streaming events, cancellation and session resume
- Usage display, logs, documentation and automated tests

MVP excludes:

- Remote/distributed workers
- Web/mobile UI
- Untrusted dynamic plugin execution
- Voice/multimodal collaboration
- Autonomous deployment to production
- Provider account management or billing resale
- Real-time multi-user collaboration

## 8. Definition of done

A feature is done only when behavior, failure paths, tests, documentation, security review and relevant acceptance criteria are complete. A demo that works only with one provider or unrestricted shell access is not an MVP completion.
