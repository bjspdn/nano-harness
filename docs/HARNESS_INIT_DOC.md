# nano: A Small Rust Agent Harness with Ratatui

For a Rust **agent harness with a Ratatui frontend**, it is useful to separate the terminal UI from the agent runtime rather than look for a single crate that does both.


## Design Principles

**nano** should be a small agent execution harness focused only on the parts that materially affect model interaction and tool execution.

Its core principles are:

- **Small core:** model requests, streaming, tool calls, context assembly, session state, and terminal rendering.
- **No assumed security boundary:** tools execute with the permissions of the nano process.
- **No built-in sandbox requirement:** if isolation is desired, the user is responsible for running nano inside a container, VM, sandbox, restricted account, or other boundary.
- **No permission prompts by default:** nano should not introduce confirmation or approval flows unless a concrete use case later requires them.
- **No speculative framework features:** no planners, multi-agent orchestration, workflow DAGs, plugin marketplaces, memory systems, or other abstractions unless the project actually needs them.
- **Transparency over abstraction:** if a behavior can be understood from a small amount of straightforward Rust, nano should prefer that over a framework layer.
- **Cache-first request construction:** prompt stability and prefix reuse are architectural constraints, not late optimizations.

The user is responsible for deciding the environment in which nano runs. nano should not pretend to provide a security boundary that it does not actually own.

A clean high-level architecture looks like this:

```text
┌─────────────────────────────────────┐
│              Ratatui UI             │
│                                     │
│ chat | tool calls | logs | status   │
└──────────────────┬──────────────────┘
                   │ events / commands
┌──────────────────▼──────────────────┐
│            Harness runtime          │
│                                     │
│ Agent loop                          │
│ Context / conversation              │
│ Tool dispatcher                     │
│ Cancellation                        │
│ Streaming                           │
│ Tracing                             │
└──────────┬──────────┬───────────────┘
           │          │
       ┌───▼───┐  ┌──▼────────┐
       │ Model │  │   Tools   │
       │ API   │  │ shell/fs/ │
       │       │  │ git/etc.  │
       └───────┘  └───────────┘
```

Ratatui should handle rendering and terminal interaction, while an asynchronous runtime such as Tokio manages model streaming, subprocess execution, filesystem access, cancellation, and background tasks.


## Cache-First Request Construction

A primary design target for nano is to keep the **cached-input-token ratio above 97% during long-running sessions whenever provider behavior allows it**.

This should be treated as an economic constraint: cached input is cheaper than repeatedly paying full input-token cost for the same conversation prefix.

The relevant metric is:

```text
cache_ratio =
    cached_input_tokens /
    total_input_tokens
```

For example:

```text
previous context: 100,000 tokens
new turn:           2,000 tokens

cache ratio ≈ 100,000 / 102,000
            ≈ 98.04%
```

nano can optimize toward this target, but cannot guarantee it because cache eviction, provider routing, model changes, minimum cacheable lengths, and provider-specific cache behavior are outside the harness.

### Preserve Exact Prefixes

Request construction should preserve the longest possible stable prefix.

Prefer:

```text
request 1:
[A][B][C]

request 2:
[A][B][C][D][E]

request 3:
[A][B][C][D][E][F][G]
```

Avoid inserting changing metadata near the beginning:

```text
request 1:
[A][B][C]

request 2:
[A][timestamp][B][C][D]

request 3:
[A][new runtime metadata][B][C][D][E]
```

Small dynamic values near the front can invalidate a large cached prefix.

### Prompt Ordering

A good default order is:

```text
1. nano system prompt
2. tool definitions
3. project instructions
4. immutable session metadata
5. conversation history
6. newest user/tool interaction
```

Dynamic information should appear as late as possible, and information that does not materially help the model should not be sent at all.

Do not place frequently changing values such as timestamps, token counters, or transient workspace state near the beginning of the prompt unless the model actually needs them.

### Usage Accounting

Provider adapters should normalize cache usage into a small common type:

```rust
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn cache_ratio(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }

        self.cached_input_tokens as f64
            / self.input_tokens as f64
    }
}
```

The TUI should expose this unobtrusively rather than building a separate analytics dashboard.

A compact status line is enough:

```text
ctx 103k · left 13.3% · cache 98.1% · out 1.4k
```



### Model-Aware Context Remaining

The TUI should also show the percentage of usable context remaining for the selected model.

This should account for both the current input size and output space reserved for the next response:

```text
ctx_left_pct =
    (context_window - estimated_input_tokens - reserved_output_tokens)
    / context_window
```

For example:

```text
context window:       128k
current input:        103k
reserved output:        8k
usable remaining:      17k

left ≈ 13.3%
```

The compact status line becomes:

```text
ctx 103k · left 13.3% · cache 98.1% · out 1.4k
```

The percentage should be derived from the currently selected model's limits rather than stored as independent session state.

A minimal representation is:

```rust
pub struct ModelLimits {
    pub context_window: u64,
    pub max_output_tokens: u64,
}
```

The UI metric can be computed with:

```rust
pub fn context_left_pct(
    input_tokens: u64,
    context_window: u64,
    reserved_output_tokens: u64,
) -> f64 {
    let used = input_tokens.saturating_add(reserved_output_tokens);
    let left = context_window.saturating_sub(used);

    left as f64 / context_window as f64 * 100.0
}
```

This metric gives nano a simple indication of context pressure without adding another panel or subsystem.


## Agent Runtime Options

There are three main approaches worth considering.

## 1. Use Rig Underneath a Custom Harness

This is a pragmatic starting point.

Rig can provide model-provider abstraction, tool calling, structured output, and reusable LLM primitives while allowing the application to own the execution loop and terminal UI.

The harness can expose an event stream such as:

```rust
enum HarnessEvent {
    UserMessage(String),

    AssistantDelta(String),
    AssistantFinished,

    ToolRequested {
        id: ToolCallId,
        name: String,
        args: serde_json::Value,
    },

    ToolStarted(ToolCallId),

    ToolOutput {
        id: ToolCallId,
        output: String,
    },

    ToolFinished(ToolCallId),

    Error(String),
}
```

Ratatui consumes these events and updates application state.

Avoid an architecture like:

```rust
agent.run().await;
draw_ui();
```

Instead, prefer a streaming event pipeline:

```text
LLM stream
    ↓
HarnessEvent::AssistantDelta
    ↓
mpsc channel
    ↓
App state
    ↓
ratatui render()
```

This keeps the UI responsive while the model is generating or a tool is executing.

---

## 2. Build the Agent Loop Yourself

For a coding-oriented harness, building the orchestration loop directly can be attractive because the core loop is relatively small.

A simplified version might look like:

```rust
loop {
    let response = model.complete(&context).await?;

    match response {
        Response::Text(text) => {
            context.push_assistant(text);
            break;
        }

        Response::ToolCalls(calls) => {
            for call in calls {
                let result = tools.execute(call).await?;
                context.push_tool_result(result);
            }
        }
    }
}
```

The value of the harness then comes from the surrounding infrastructure:

```text
Harness
 ├─ Model
 ├─ ContextManager
 ├─ ToolRegistry
 ├─ Executor
 ├─ Workspace
 ├─ Session
 └─ EventBus
```

A possible core type:

```rust
pub struct Harness<M> {
    model: M,
    tools: ToolRegistry,
    context: ContextManager,
    workspace: Workspace,
    events: broadcast::Sender<HarnessEvent>,
}
```

Tools can be represented behind a small async trait:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn schema(&self) -> serde_json::Value;

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: serde_json::Value,
    ) -> anyhow::Result<ToolResult>;
}
```

This keeps the abstraction surface small and avoids coupling the application to another framework's definition of an `Agent`.

---

## 3. Use a Graph or Workflow Runtime

A graph-based runtime becomes useful when the workflow is explicitly multi-stage or multi-agent.

For example:

```text
planner
  ↓
coder
  ↓
tests
  ↓
reviewer
  ├── pass → done
  └── fail → coder
```

For a terminal coding agent, this is usually unnecessary at the beginning.

A conventional event-driven state machine is simpler to debug and reason about.


## Initial Provider Scope

nano should initially target a deliberately small provider set:

```text
OpenRouter
OpenAI API
OpenCode Go
```

Codex support can be added later.

The provider abstraction should remain thin:

```rust
#[async_trait]
pub trait Provider {
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<ModelStream>;
}
```

A simple module layout is enough:

```text
src/provider/
├── mod.rs
├── openrouter.rs
├── openai.rs
└── opencode_go.rs
```

Avoid adding routing middleware, fallback systems, capability negotiation, provider registries, or other abstractions until a real requirement appears.

Normalize only the fields nano genuinely needs:

```rust
pub struct ModelRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub cache_key: Option<String>,
}
```

And stream a small common event model:

```rust
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Done,
}
```

Provider-specific features should remain inside their respective adapters unless they become useful across multiple providers.


## Ratatui Application Architecture

The UI can be modeled as a projection of central application state.

```rust
pub struct App {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolExecution>,
    pub input: String,
    pub status: AgentStatus,
    pub active_panel: Panel,
    pub scroll: usize,
}
```

There should ideally be one rendering loop responsible for both terminal input and harness events:

```rust
loop {
    tokio::select! {
        Some(event) = harness_rx.recv() => {
            app.update(event);
        }

        Some(event) = terminal_rx.recv() => {
            app.handle_input(event);
        }

        _ = tick.tick() => {}
    }

    terminal.draw(|frame| ui::render(frame, &app))?;
}
```

This matches Ratatui's immediate-mode rendering model: each frame is reconstructed from current application state.

## Example Terminal Layout

A minimal agent interface might look like:

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
│ ▸ rg "validate_token" src           │
│   ✓ 4 matches                       │
│                                     │
│ The issue appears to be...          │
│                                     │
├─────────────────────────────────────┤
│ ctx 103k · left 13.3% · cache 98.1% · out 1.4k   │
├─────────────────────────────────────┤
│ > _                                 │
└─────────────────────────────────────┘
```

As the product grows, optional side panels can expose context and workspace state:

```text
┌──────────────────────┬──────────────┐
│ Conversation         │ Context      │
│                      │              │
│                      │ 42k / 128k   │
│                      │              │
│                      ├──────────────┤
│                      │ Modified     │
│                      │ src/auth.rs  │
│                      │ src/lib.rs   │
├──────────────────────┴──────────────┤
│ > prompt...                         │
└─────────────────────────────────────┘
```

## Suggested Crates

A starting dependency set could look roughly like:

```toml
[dependencies]
ratatui = "..."
crossterm = "..."
tokio = { version = "...", features = ["full"] }

serde = { version = "...", features = ["derive"] }
serde_json = "..."

anyhow = "..."
thiserror = "..."

async-trait = "..."
futures = "..."

tracing = "..."
tracing-subscriber = "..."

uuid = { version = "...", features = ["v4"] }
```

If Rig is used for model and tool-call abstractions:

```toml
rig-core = "..."
```

An alternative is to implement provider adapters directly and keep the harness independent of any agent framework.

## Prefer `Session` and `Run` Over `Agent`

For a coding harness, `Agent` does not necessarily need to be the central domain abstraction.

A more flexible model is:

```text
Session
 ├── Messages
 ├── ModelConfig
 ├── Workspace
 ├── Context
 └── Runs
      └── Run
          ├── LLM requests
          ├── Tool calls
          ├── Tool results
          ├── Permissions
          └── Artifacts
```

This structure naturally supports:

- Replayability
- Persistence
- Debugging
- Cancellation
- Session branching
- Multiple runs per conversation
- Tool-call inspection
- Multi-agent execution later

It also keeps the application's persistence and observability model independent from any particular LLM framework.

## Recommended Direction

For nano, the preferred starting architecture is intentionally small:

```text
ratatui
   +
tokio
   +
custom harness/event loop
   +
thin provider adapters
```

Rig can still be evaluated as an implementation detail, but nano should not make a third-party `Agent` abstraction central to its architecture.

The important architectural boundary is that **the harness owns execution and state**, while Ratatui only renders state and emits commands.


The core should remain close to:

```text
nano
├── session
├── prompt
├── provider
├── tools
├── runtime
└── tui
```

There should be no `PermissionManager`, `SandboxManager`, `WorkflowEngine`, `AgentRegistry`, or similar subsystem unless a concrete requirement later justifies one.


A possible project structure could eventually look like:

```text
src/
├── main.rs
├── app/
│   ├── mod.rs
│   ├── state.rs
│   └── command.rs
├── ui/
│   ├── mod.rs
│   ├── chat.rs
│   ├── tools.rs
│   ├── input.rs
│   └── status.rs
├── runtime/
│   ├── mod.rs
│   ├── harness.rs
│   ├── event.rs
│   └── run.rs
├── model/
│   ├── mod.rs
│   ├── provider.rs
│   └── response.rs
├── tools/
│   ├── mod.rs
│   ├── registry.rs
│   ├── shell.rs
│   ├── filesystem.rs
│   └── git.rs
├── session/
│   ├── mod.rs
│   ├── session.rs
│   └── persistence.rs
├── workspace/
│   ├── mod.rs
│   └── workspace.rs
└── permissions/
    ├── mod.rs
    └── policy.rs
```

This gives the project a clear separation between UI, orchestration, model integration, tools, persistent session state, workspace operations, and permissions.


## System Prompts and Prompt Composition

System prompts should be treated as **part of the harness configuration**, not as something Ratatui or the agent loop implicitly manages.

At runtime, the harness typically assembles a model request from several distinct inputs:

```text
system prompt
+ session context
+ conversation history
+ tool definitions
+ current user message
```

A useful decomposition is:

```text
SystemPrompt
├── Base policy
├── Agent role
├── Tool-use instructions
├── Workspace instructions
├── Safety / permission rules
└── Runtime context
```

A simple representation might look like:

```rust
pub struct SystemPrompt {
    pub base: String,
    pub role: String,
    pub tool_policy: String,
    pub workspace_context: String,
}
```

The final system message can then be rendered from those components:

```rust
impl SystemPrompt {
    pub fn render(&self) -> String {
        format!(
            r#"
{base}

# Role
{role}

# Tool Policy
{tool_policy}

# Workspace
{workspace_context}
"#,
            base = self.base,
            role = self.role,
            tool_policy = self.tool_policy,
            workspace_context = self.workspace_context,
        )
    }
}
```

### Prefer Prompt Layers Over One Giant Static Prompt

For a coding harness, it is better to separate static behavioral instructions from dynamic environment information.

```text
Static
├── "You are a coding agent..."
├── Formatting rules
├── Behavioral rules
└── Tool-use semantics

Dynamic
├── Current working directory
├── Repository metadata
├── Git branch
├── Available tools
├── Permission mode
├── Project instructions
└── Current environment
```

This makes prompt composition easier to inspect, test, cache, and evolve.

A request builder might look like:

```rust
pub struct RequestBuilder {
    system: SystemPromptBuilder,
    context: ContextManager,
    tools: ToolRegistry,
}

impl RequestBuilder {
    pub fn build(&self, input: &str) -> ModelRequest {
        ModelRequest {
            system: self.system.build(),
            messages: self.context.messages(),
            tools: self.tools.schemas(),
            user_input: input.to_owned(),
        }
    }
}
```

### Keep System Prompts Separate From Session State

The system prompt should contain stable or semi-stable behavioral instructions.

It should not become a dumping ground for conversation history.

Avoid:

```text
System:
You are a coding agent.

The user previously asked X.
Then you edited Y.
Then command Z failed.
Then...
```

Prefer:

```text
System:
Stable behavioral instructions

Messages:
Conversation history

Tool results:
Structured execution results

Context:
Selected repository information
```

This separation is important for token budgeting, replayability, summarization, and debugging.

### Model Prompt Layers Explicitly

A harness can represent prompt composition as explicit layers:

```rust
pub enum PromptLayer {
    Core,
    AgentRole,
    ToolInstructions,
    ProjectInstructions,
    RuntimeContext,
}
```

For example:

```rust
pub struct PromptStack {
    pub layers: Vec<PromptLayerContent>,
}
```

This gives the runtime a structured representation of where each instruction came from.

It also makes it possible to inspect prompt composition in the TUI:

```text
┌─ System Prompt ─────────────────────┐
│ Core                1,240 tokens    │
│ Tool policy           630 tokens    │
│ AGENTS.md              410 tokens   │
│ Runtime context        190 tokens   │
│                                    │
│ Total                2,470 tokens   │
└────────────────────────────────────┘
```

This can be extremely useful when debugging agent behavior.

### Project-Local Instructions

A coding harness should support repository-local instructions.

Common examples include:

```text
AGENTS.md
CLAUDE.md
.agent/instructions.md
```

The workspace subsystem can discover and load one or more of these files:

```rust
let project_instructions =
    workspace.load_agent_instructions().await?;
```

They can then be injected as their own prompt layer:

```text
System prompt
├── Harness core prompt
├── Tool contract
├── Project instructions
└── Runtime metadata
```

Keeping project instructions as a distinct layer makes them easier to inspect and lets the harness define precedence rules explicitly.

### Tool Instructions vs Tool Schemas

The system prompt should explain **when and why** tools should be used.

Tool schemas should explain **what arguments** a tool accepts.

For example, the prompt might contain:

```text
Use filesystem tools to inspect files before modifying them.
Do not assume file contents.

Before executing destructive shell commands, request permission.

After editing code, run the most relevant available validation command.
```

The actual tool definition is passed separately:

```json
{
  "name": "read_file",
  "description": "Read a file from the workspace",
  "parameters": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string"
      }
    },
    "required": ["path"]
  }
}
```

Avoid duplicating full tool schemas inside the prompt unless a provider requires it.

### Prompt Compilation

A useful long-term abstraction is a prompt compiler:

```text
Prompt sources
      ↓
PromptBuilder
      ↓
PromptStack
      ↓
Token budgeting
      ↓
Provider-specific ModelRequest
```

This matters because different model providers expose somewhat different request structures.

Conceptually:

```text
OpenAI       → instructions/messages/tools
Anthropic    → system/messages/tools
Gemini       → system_instruction/contents/tools
```

The harness can normalize those differences behind a provider-neutral request type:

```rust
pub struct ModelRequest {
    pub instructions: Instructions,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}
```

Each provider adapter then translates that internal representation to the provider-specific API format.

### Token Budgeting

Prompt composition should be integrated with context-window management.

A useful budget might be divided between:

```text
Context window
├── System / developer instructions
├── Tool definitions
├── Project instructions
├── Recent conversation
├── Retrieved workspace context
├── Tool results
└── Reserved output tokens
```

The context manager should know the approximate token cost of each prompt layer.

This makes it possible to prioritize important instructions while trimming lower-value context when the model approaches its context limit.

### Store the Rendered Prompt for Every Run

For reproducibility and debugging, persist the exact rendered prompt that was sent to the model.

For example:

```rust
pub struct Run {
    pub id: RunId,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ToolCall>,
    pub model: ModelId,
}
```

A richer implementation may also persist:

```rust
pub struct PromptSnapshot {
    pub rendered: String,
    pub layers: Vec<PromptLayerSnapshot>,
    pub token_count: usize,
    pub project_instructions_hash: String,
}
```

This allows a run to be inspected later without depending on the current version of the prompt builder.

It also makes prompt changes observable: when agent behavior changes after a release, the harness can compare the exact prompt snapshots between runs.

### Suggested Prompt Module Layout

The project structure can be extended with a dedicated prompt subsystem:

```text
src/
├── prompt/
│   ├── mod.rs
│   ├── builder.rs
│   ├── layer.rs
│   ├── project.rs
│   ├── budget.rs
│   └── snapshot.rs
```

The overall flow becomes:

```text
Workspace
   │
   ├── AGENTS.md
   ├── Git metadata
   └── Environment
           │
           ▼
     PromptBuilder
           │
           ▼
      PromptStack
           │
           ▼
     ContextManager
           │
           ▼
      ModelRequest
```

The key architectural principle is to keep **prompt composition**, **context management**, and **tool policy** as separate subsystems.

They interact closely, but keeping them distinct makes the harness easier to reason about, test, debug, and extend.
