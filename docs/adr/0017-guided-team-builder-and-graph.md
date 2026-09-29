# ADR 0017: Guided team builder and interactive developer graph

The graph-rendering implementation in this decision is superseded by ADR 0021.

## Context

The configuration schema already supports arbitrary formations and per-agent
provider/model routes, but the first-run experience exposed only fixed solo or
three-agent templates. The workspace rendered agents as dense text in a narrow
panel, which made team composition and delegation relationships difficult to
understand. Issue #5 calls for a characterful terminal experience, visible
connecting threads, and direct mouse movement of developer cards without
losing keyboard operation.

## Decision

When the executable is launched without a subcommand and the workspace config
does not exist, the application runs a Ratatui team builder before composing
the engine. A quick-defaults page collects the bounded developer count, an
existing working directory defaulting to `.`, and one default maximum-output-token limit. Developer names
and roles are suggested; each developer page exposes only role, provider, and
model, and Enter accepts the suggestion. Model choices
and provider-qualified prices are supplied by the bundled, dated models.dev
snapshot through `ai-model-directory-router`; compatible OpenAI offerings are
filtered to text/tool-capable models. Installed Ollama models are discovered
through the existing provider adapter with a bounded timeout because a static
catalog cannot describe a user's local runtime. Catalog metadata stays in the
application/UI layer and does not enter core domain logic. The token limit is
copied to each provider-neutral agent profile and then into every
canonical provider request owned by that developer. The resulting version 1
configuration is validated through the existing configuration boundary and
created with no-overwrite semantics. If OpenAI is selected and no credential is
already resolvable, the builder offers one optional masked API token field for
secure native storage; skipping it does not block launch. Existing configs
default to 4096 output
tokens, and explicit CLI commands retain their previous behavior;
non-interactive setup remains available through `voff init`.

Usage events are costed against the task owner's provider/model with the
catalog crate's decimal calculator. User-configured input/output rates override
catalog pricing. Missing local or catalog prices remain unknown, and totals
that omit unpriced usage are labelled partial; missing data is never treated as
zero cost.

The runtime workspace projects persisted `AgentProfile` values into a card
graph. Faint edges come only from explicit `allowed_targets`; free-form model
text is never interpreted as a relationship. A persisted child task whose
parent has a different owner promotes the matching route into a bright,
directional edge labelled with the child's objective. Running routes add an
animated travelling marker, and running role cards use the application-owned
render tick with `ratatui-spinner` while retaining the textual `working` label.
Routes use explicit node ports and remain solid after completion, with textual
working, approval, completed, or failed labels on the edge. The graph title
summarizes active or completed handoffs so narrow terminals do not rely on
color or a transient animation. Each card retains its latest task after a
terminal transition instead of reverting immediately to an apparently idle
developer. These animations
are pure TUI projections of structured task ownership and never imply engine
state. Mouse hit-testing and drag offsets live only in the TUI projection, so
moving a card cannot mutate formation policy or domain state. Keyboard
navigation, text labels, narrow-terminal layout, approvals, and event
inspection remain available.

## Consequences

First launch is substantially more discoverable and heterogeneous teams no
longer require hand-editing TOML. The graph makes delegation authority and live
work visible while preserving provider neutrality and the existing bounded
engine channels. Card positions are intentionally session-local presentation
state; persisting layouts can be added later without changing task or agent
semantics. The spinner dependency is pinned to a Git revision while the Ratatui
project keeps it unpublished. Very large formations use increasingly compact cards, so future
zoom and viewport controls may improve inspection without changing this
decision. The bundled catalog increases binary size and becomes dated between
dependency updates, but preserves deterministic, offline startup and records
its retrieval timestamp in the setup UI.
