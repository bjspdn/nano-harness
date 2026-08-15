# nano Implementation Roadmap

## Goal

Build **nano** as a small Rust agent harness with a Ratatui frontend.

nano should focus only on the pieces required to run a useful coding agent:

- model requests
- streaming responses
- tool calls
- session state
- context management
- prompt composition
- provider adapters
- cache-aware request construction
- a minimal terminal UI

nano should not add security boundaries, permission prompts, workflow engines, multi-agent systems, plugin marketplaces, memory subsystems, or other abstractions unless a concrete need appears later.

The user is responsible for deciding whether nano runs directly on the host or inside a sandbox.

---

# Phase 0 — Project Skeleton

## Objective

Establish the smallest useful Rust application structure without committing to unnecessary abstractions.

## Features

- [x] Create Rust workspace / crate
- [x] Add `tokio`
- [x] Add `ratatui`
- [x] Add `crossterm`
- [x] Add `serde` / `serde_json`
- [x] Add `thiserror` or `anyhow`
- [x] Add `tracing`
- [x] Add basic CLI startup
- [x] Enter and restore terminal mode cleanly
- [x] Handle Ctrl-C and terminal shutdown correctly

## Initial Module Layout

```text
src/
├── main.rs
├── tui/
├── runtime/
├── provider/
├── prompt/
├── session/
└── tools/
```

## Exit Criteria

Running `nano` opens an empty Ratatui application and exits cleanly without leaving the terminal in a broken state.

---

# Phase 1 — Minimal TUI

## Objective

Create the smallest usable interactive shell around the harness.

## Features

- [x] Conversation viewport
- [x] Prompt input box
- [x] Submit user messages
- [x] Scroll conversation history
- [x] Display assistant text
- [x] Display current model
- [x] Display basic runtime status
- [x] Handle terminal resize
- [x] Keep rendering independent from model execution

## Target Layout

```text
┌─ nano ───────────────────────────────┐
│                                     │
│ You: inspect the authentication bug │
│                                     │
│ Assistant: I'll inspect...          │
│                                     │
│ ▸ read_file src/auth.rs             │
│   ✓ 183 lines                       │
│                                     │
│ The issue appears to be...          │
│                                     │
├─────────────────────────────────────┤
│ ctx 103k · left 13.3% · cache 98.1% │
├─────────────────────────────────────┤
│ > _                                 │
└─────────────────────────────────────┘
```

The status line can later include output-token usage if it remains readable:

```text
ctx 103k · left 13.3% · cache 98.1% · out 1.4k
```

## Architecture

The TUI should consume events and render application state.

It should not call providers directly.

```text
runtime
   ↓
HarnessEvent
   ↓
AppState
   ↓
Ratatui
```

## Exit Criteria

The user can type a message, submit it, and see a mock assistant response while the interface remains responsive.

---

# Phase 2 — Provider Interface

## Objective

Define the smallest provider-neutral API nano needs.

## Features

- [x] `Provider` trait
- [x] `ModelRequest`
- [x] streaming model events
- [x] text deltas
- [x] tool-call events
- [x] usage reporting
- [x] provider errors
- [x] model metadata
- [x] model context-window limits

## Suggested Core Interface

```rust
#[async_trait]
pub trait Provider {
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<ModelStream>;
}
```

## Suggested Stream Events

```rust
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Done,
}
```

Do not add capability negotiation or generalized middleware until it is actually needed.

## Exit Criteria

A fake provider can stream text through the runtime into the TUI.

---

# Phase 3 — OpenAI API Provider

## Objective

Implement one provider end to end before generalizing further.

## Features

- [ ] API authentication
- [ ] model selection
- [ ] request serialization
- [ ] streaming responses
- [ ] tool definitions
- [ ] tool-call parsing
- [ ] usage parsing
- [ ] cached-input-token parsing
- [ ] context-window metadata
- [ ] provider error mapping

## Exit Criteria

nano can hold a real streaming conversation through the OpenAI API and display usage data in the TUI.

---

# Phase 4 — Session Model

## Objective

Introduce persistent in-memory conversation state.

## Features

- [ ] `Session`
- [ ] ordered messages
- [ ] assistant messages
- [ ] user messages
- [ ] tool calls
- [ ] tool results
- [ ] model selection
- [ ] run IDs
- [ ] request usage
- [ ] context-token estimate

## Suggested Shape

```text
Session
├── model
├── messages
└── runs
    └── Run
        ├── request
        ├── response
        ├── tool calls
        └── usage
```

Do not add database persistence yet unless it becomes necessary for actual use.

## Exit Criteria

Each turn is represented as structured session state rather than raw strings attached directly to the TUI.

---

# Phase 5 — Prompt Stack

## Objective

Build prompts explicitly and predictably.

## Features

- [ ] core nano system prompt
- [ ] tool-use instructions
- [ ] project instructions
- [ ] runtime context
- [ ] prompt layers
- [ ] rendered prompt snapshot
- [ ] deterministic ordering

## Default Prompt Order

```text
1. nano system prompt
2. tool definitions
3. project instructions
4. immutable session metadata
5. conversation history
6. newest interaction
```

## Project Instructions

Support repository-local instructions without creating a broader configuration framework.

Initial candidates:

```text
AGENTS.md
CLAUDE.md
.agent/instructions.md
```

A later decision can define precedence if multiple files exist.

## Exit Criteria

Given identical inputs, nano produces identical prompt prefixes.

---

# Phase 6 — Cache-First Request Construction

## Objective

Make prompt caching a core runtime concern.

## Primary Target

Aim for:

```text
cached_input_tokens / input_tokens > 97%
```

during sufficiently long-running sessions when the provider supports prompt caching and the request shape allows it.

This is a design target, not a guarantee.

## Features

- [ ] stable prompt prefix
- [ ] append-only conversation history where possible
- [ ] no timestamps in stable prefix
- [ ] no changing runtime data near the beginning
- [ ] normalized cached-token usage
- [ ] cache-ratio calculation
- [ ] cache-ratio display in TUI
- [ ] optional provider cache-key support where available

## Usage Type

```rust
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}
```

## Cache Metric

```text
cache_ratio =
    cached_input_tokens /
    input_tokens
```

## Exit Criteria

nano reports actual provider cache usage and makes it easy to detect regressions in prefix stability.

---

# Phase 7 — Context Accounting

## Objective

Make context pressure visible and deterministic.

## Features

- [ ] model context-window metadata
- [ ] input-token estimate
- [ ] reserved output-token budget
- [ ] remaining usable context
- [ ] remaining-context percentage
- [ ] TUI status display

## Calculation

```text
ctx_left_pct =
    (
        context_window
        - estimated_input_tokens
        - reserved_output_tokens
    )
    / context_window
```

## Suggested Type

```rust
pub struct ModelLimits {
    pub context_window: u64,
    pub max_output_tokens: u64,
}
```

## Exit Criteria

The TUI accurately shows context usage for the selected model:

```text
ctx 103k · left 13.3% · cache 98.1%
```

---

# Phase 8 — Tool Runtime

## Objective

Allow the model to call local tools through a very small execution layer.

## Initial Tools

Start only with tools that materially help coding tasks.

- [ ] `read_file`
- [ ] `write_file`
- [ ] `list_dir`
- [ ] `shell`

Potentially add later:

- [ ] `grep` / search
- [ ] git inspection

Do not create specialized tools if shell execution already covers the use case cleanly.

## Tool Interface

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

## Important Constraint

nano does **not** add permission prompts or sandboxing.

Tools execute with the privileges of the nano process.

The user chooses whether to run nano inside an external security boundary.

## Exit Criteria

The model can request a tool call, nano executes it, appends the result to the conversation, and continues the model loop.

---

# Phase 9 — Agent Loop

## Objective

Connect model responses and tool execution into the core harness loop.

## Flow

```text
user input
    ↓
build request
    ↓
provider stream
    ↓
tool call?
 ┌──┴───┐
 no    yes
 │       │
done   execute
         │
         └── append result
                 ↓
              provider
```

## Features

- [ ] user turn
- [ ] streamed assistant output
- [ ] multiple tool calls
- [ ] tool results
- [ ] continue-after-tool-call
- [ ] clean run completion
- [ ] cancellation
- [ ] runtime error reporting

## Exit Criteria

nano can complete a full coding-agent turn involving multiple tool calls without blocking the TUI.

---

# Phase 10 — Tool Activity in the TUI

## Objective

Expose enough execution state to understand what nano is doing.

## Features

- [ ] tool requested
- [ ] tool running
- [ ] tool completed
- [ ] concise tool result summary
- [ ] failures
- [ ] active model state
- [ ] streaming indicator

Example:

```text
▸ read_file src/auth.rs
  ✓ 183 lines

▸ rg "validate_token" src
  ✓ 4 matches
```

Avoid adding separate dashboards or complex observability panes.

## Exit Criteria

The user can understand the current run without reading logs.

---

# Phase 11 — OpenRouter Provider

## Objective

Add OpenRouter without changing the core runtime model.

## Features

- [ ] OpenRouter authentication
- [ ] model selection
- [ ] streaming
- [ ] tools
- [ ] usage
- [ ] cache-read / cache-write information where exposed
- [ ] context-window metadata

## Constraint

If OpenRouter exposes provider-specific behavior, keep it inside the adapter unless at least one other provider needs the same abstraction.

## Exit Criteria

Switching between OpenAI and OpenRouter does not require changes to session, prompt, tool, or TUI code.

---

# Phase 12 — OpenCode Go Provider

## Objective

Add OpenCode Go as the third initial provider.

## Features

- [ ] authentication
- [ ] model discovery or configuration
- [ ] request mapping
- [ ] streaming
- [ ] tools
- [ ] usage
- [ ] model limits

## Exit Criteria

nano supports OpenAI, OpenRouter, and OpenCode Go through the same minimal runtime abstraction.

---

# Phase 13 — Configuration

## Objective

Add only the configuration users actually need.

## Initial Configuration

- [ ] provider
- [ ] model
- [ ] API credentials through environment variables
- [ ] reserved output-token budget
- [ ] project instruction file behavior

Possible format:

```toml
provider = "openai"
model = "..."
reserved_output_tokens = 8192
```

Avoid a large configuration schema.

Environment variables should remain sufficient for secrets.

## Exit Criteria

nano can be configured without recompilation while keeping the configuration surface small.

---

# Phase 14 — Session Persistence

## Objective

Add persistence only after the runtime model is stable.

## Features

- [ ] save session
- [ ] restore session
- [ ] preserve messages
- [ ] preserve model
- [ ] preserve run usage
- [ ] preserve prompt snapshots where useful

Prefer a simple local format before introducing a database.

Candidates:

- JSON
- JSONL
- MessagePack

## Exit Criteria

nano can exit and resume a previous conversation without losing the model-visible history.

---

# Phase 15 — Context Compaction

## Objective

Handle sessions that approach the model context limit.

This should be implemented only after context accounting is working reliably.

## Features

- [ ] detect low remaining context
- [ ] define compaction threshold
- [ ] summarize or replace old context
- [ ] preserve important tool / project information
- [ ] start a new stable cache prefix after compaction
- [ ] clearly expose compaction in session state

## Constraint

Do not build a generalized memory system.

Compaction exists only to keep long-running sessions usable.

## Exit Criteria

A long-running session can continue after approaching the context limit without silently exceeding the selected model's window.

---

# Phase 16 — Reliability

## Objective

Make the small core robust.

## Features

- [ ] HTTP timeouts
- [ ] cancellation
- [ ] recoverable streaming failures
- [ ] tool execution failures
- [ ] malformed provider response handling
- [ ] malformed tool-call handling
- [ ] terminal restoration after panic where practical
- [ ] structured tracing

Avoid automatic behavior that hides failures.

Errors should remain understandable to the user.

## Exit Criteria

Provider or tool failures do not leave nano in an inconsistent session or terminal state.

---

# Phase 17 — Codex Provider

## Objective

Add Codex support after the core provider model has proven stable.

## Features

To be defined when Codex integration becomes a concrete implementation target.

The provider should fit nano's existing abstractions rather than forcing speculative abstractions into the core ahead of time.

---

# Explicit Non-Goals

Unless concrete requirements emerge, nano should **not** implement:

- built-in sandboxing
- permission prompts
- confirmation dialogs for tool execution
- agent permission policies
- workflow DAGs
- planner / executor frameworks
- multi-agent orchestration
- agent registries
- plugin marketplaces
- general-purpose memory systems
- vector databases
- RAG frameworks
- autonomous background agents
- remote execution infrastructure
- telemetry dashboards
- elaborate provider fallback logic
- generalized provider capability negotiation
- GUI support
- web UI support

These can always be reconsidered later.

They should not influence the initial architecture.

---

# MVP Definition

nano reaches its first meaningful MVP when it can:

- [ ] launch a Ratatui interface
- [ ] accept a user prompt
- [ ] stream a response from one real provider
- [ ] maintain session history
- [ ] compose a deterministic system prompt
- [ ] execute basic coding tools
- [ ] continue the model loop after tool calls
- [ ] report token usage
- [ ] report cache-hit ratio
- [ ] report remaining context percentage
- [ ] preserve a cache-friendly prompt prefix
- [ ] cancel a running request
- [ ] exit cleanly

At that point, nano is already a useful agent harness.

Everything after that should be justified by actual usage.

---

# Recommended Implementation Order

```text
1. project skeleton
2. minimal Ratatui UI
3. provider interface
4. OpenAI provider
5. session model
6. prompt stack
7. cache accounting
8. context accounting
9. tool runtime
10. agent loop
11. tool activity UI
12. OpenRouter
13. OpenCode Go
14. configuration
15. persistence
16. context compaction
17. reliability hardening
18. Codex
```

The order intentionally favors a complete vertical slice over broad framework design.

The first target should be:

```text
user
 ↓
Ratatui
 ↓
nano runtime
 ↓
OpenAI
 ↓
streamed response
```

Then add tools:

```text
user
 ↓
Ratatui
 ↓
nano runtime
 ↓
model
 ↓
tool call
 ↓
local execution
 ↓
tool result
 ↓
model
 ↓
response
```

Once that loop works cleanly, additional providers and persistence become incremental work rather than architectural work.
