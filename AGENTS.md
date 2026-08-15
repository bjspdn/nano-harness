# AGENTS.md

## Project

`nano` is a small Rust agent harness with a Ratatui frontend.

Keep it small, explicit, and easy to reason about.

## Core Rules

- Prefer **composition over inheritance-like hierarchies**.
- Build small components with narrow responsibilities.
- Components should be independently removable whenever practical.
- Removing an optional TUI component should not break unrelated components.
- Avoid global state.
- Avoid hidden coupling between modules.
- Prefer explicit data flow over implicit behavior.
- Prefer plain Rust types and traits over framework abstractions.
- Do not add features without a concrete requirement.
- Do not introduce abstractions for hypothetical future use.

## TUI Architecture

The TUI should be assembled from small components.

Each component should:

- render from explicit state
- handle only the events it owns
- avoid directly mutating unrelated component state
- communicate through shared application events or narrowly scoped interfaces
- remain testable independently where practical

Prefer:

```text
App
├── Conversation
├── Input
├── StatusLine
└── ToolActivity
```

over deep component hierarchies.

Optional components should degrade cleanly.

For example, removing `ToolActivity` should not affect conversation rendering, input handling, provider execution, or session state.

## Runtime Boundaries

Keep these concerns separate:

```text
tui
runtime
provider
prompt
session
tools
```

The TUI must not call providers directly.

Providers must not know about Ratatui.

Tools must not depend on TUI state.

Prompt construction must not depend on rendering concerns.

## Minimalism

Do not add:

- built-in sandboxing
- permission prompts
- workflow engines
- multi-agent orchestration
- plugin systems
- memory frameworks
- generalized middleware
- provider capability frameworks
- dashboards

unless a real requirement justifies them.

The user is responsible for running nano inside a sandbox if desired.

## Prompt and Cache Rules

Prompt construction is cache-first.

- Preserve stable prefixes.
- Keep static instructions before dynamic content.
- Do not insert timestamps or frequently changing metadata near the beginning.
- Keep tool definitions deterministic.
- Keep project instructions deterministic.
- Append new conversation state whenever possible.
- Track cached input tokens.
- Aim for a cache ratio above 97% in sufficiently long sessions where provider behavior permits it.

## Context Rules

Track:

- current input tokens
- model context-window size
- reserved output tokens
- remaining usable context percentage
- cached input ratio

Keep the TUI representation compact.

Example:

```text
ctx 103k · left 13.3% · cache 98.1% · out 1.4k
```

## Provider Rules

Initial providers:

- OpenRouter
- OpenAI API
- OpenCode Go

Codex can be added later.

Keep provider adapters thin.

Provider-specific behavior stays inside the provider implementation unless multiple providers require the same abstraction.

## Tool Rules

Tools should be small and explicit.

Prefer a narrow interface:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> serde_json::Value;

    async fn execute(
        &self,
        args: serde_json::Value,
    ) -> Result<ToolResult>;
}
```

Do not add permission or sandbox layers around tools.

Tools execute with the permissions of the nano process.

## Dependency Rules

Before adding a dependency:

1. Check whether the standard library or an existing dependency is sufficient.
2. Confirm the dependency removes meaningful complexity.
3. Avoid large frameworks for small problems.
4. Prefer crates with a narrow purpose.

## Change Rules

When modifying nano:

- keep diffs focused
- avoid unrelated refactors
- preserve module boundaries
- avoid increasing coupling
- remove dead abstractions
- keep public APIs small
- prefer deleting complexity over documenting unnecessary complexity

## Design Test

Before adding a new abstraction, ask:

> Can this be implemented clearly with a small Rust type, trait, function, or component instead?

If yes, prefer the smaller solution.
