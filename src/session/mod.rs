//! Conversation session state.

use thiserror::Error;

use crate::provider::{CompletionOutcome, ModelMessage, ModelRequest, ToolCall, Usage};

/// The deterministic identity of one session run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(u64);

impl RunId {
    /// Return the session-local numeric value of this run identity.
    #[cfg(test)]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

/// The deterministic identity of one conversation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    /// Return the session-local numeric value of this message identity.
    #[cfg(test)]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

/// The category of a provider failure retained by a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    RequestSetup,
    Streaming,
    IncompleteStream,
}

/// Provider-neutral failure data retained on a failed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFailure {
    category: FailureCategory,
    display_detail: String,
}

impl SessionFailure {
    #[cfg(test)]
    pub fn category(&self) -> FailureCategory {
        self.category
    }

    #[cfg(test)]
    pub fn display_detail(&self) -> &str {
        &self.display_detail
    }
}

/// The terminal state of a run, including the active state before termination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTerminalState {
    Running,
    Completed(CompletionOutcome),
    Failed(SessionFailure),
}

/// One canonical message and its session-local ownership metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRecord {
    message_id: MessageId,
    run_id: RunId,
    message: ModelMessage,
    model_visible: bool,
}

impl ConversationRecord {
    #[cfg(test)]
    pub fn message_id(&self) -> MessageId {
        self.message_id
    }

    #[cfg(test)]
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn message(&self) -> &ModelMessage {
        &self.message
    }

    #[cfg(test)]
    pub fn is_model_visible(&self) -> bool {
        self.model_visible
    }
}

/// The provider request and accumulated response identity for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExchange {
    request: ModelRequest,
    assistant_message_id: Option<MessageId>,
    usage: Option<Usage>,
    terminal_state: RunTerminalState,
}

impl ProviderExchange {
    #[cfg(test)]
    pub fn request(&self) -> &ModelRequest {
        &self.request
    }

    #[cfg(test)]
    pub fn assistant_message_id(&self) -> Option<MessageId> {
        self.assistant_message_id
    }

    #[cfg(test)]
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    #[cfg(test)]
    pub fn terminal_state(&self) -> &RunTerminalState {
        &self.terminal_state
    }
}

/// One submitted user request and its single phase-4 provider exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    run_id: RunId,
    model_id: String,
    exchanges: Vec<ProviderExchange>,
    terminal_state: RunTerminalState,
}

impl Run {
    pub fn id(&self) -> RunId {
        self.run_id
    }

    #[cfg(test)]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[cfg(test)]
    pub fn exchanges(&self) -> &[ProviderExchange] {
        &self.exchanges
    }

    #[cfg(test)]
    pub fn terminal_state(&self) -> &RunTerminalState {
        &self.terminal_state
    }
}

/// Errors returned when a session transition cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    #[error("run ID counter is exhausted")]
    RunIdExhausted,
    #[error("message ID counter is exhausted")]
    MessageIdExhausted,
    #[error("run {run_id:?} does not exist")]
    UnknownRun { run_id: RunId },
    #[error("run {run_id:?} is still active")]
    RunInProgress { run_id: RunId },
    #[error("run {run_id:?} is already terminal")]
    RunAlreadyTerminal { run_id: RunId },
    #[error("invalid conversation protocol: {detail}")]
    InvalidProtocol { detail: String },
    #[allow(dead_code)]
    #[error("tool call ID cannot be empty")]
    EmptyToolCallId,
    #[allow(dead_code)]
    #[error("tool call ID already exists: {tool_call_id}")]
    DuplicateToolCallId { tool_call_id: String },
    #[allow(dead_code)]
    #[error("tool result references missing tool call {tool_call_id} in run {run_id:?}")]
    MissingToolCall { run_id: RunId, tool_call_id: String },
    #[allow(dead_code)]
    #[error(
        "tool result for run {run_id:?} is out of order: expected {expected_tool_call_id}, received {actual_tool_call_id}"
    )]
    ToolResultOutOfOrder {
        run_id: RunId,
        expected_tool_call_id: String,
        actual_tool_call_id: String,
    },
    #[allow(dead_code)]
    #[error("tool call {tool_call_id} in run {run_id:?} is already resolved")]
    ToolCallAlreadyResolved { run_id: RunId, tool_call_id: String },
    #[error("tool call {tool_call_id} in run {run_id:?} remains unresolved")]
    UnresolvedToolCall { run_id: RunId, tool_call_id: String },
}

/// The committed data handed to runtime when a run starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStart {
    run_id: RunId,
    user_message_id: MessageId,
    request: ModelRequest,
}

impl RunStart {
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn user_message_id(&self) -> MessageId {
        self.user_message_id
    }

    pub fn request(&self) -> &ModelRequest {
        &self.request
    }
}

/// The assistant identity established by one text-delta transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssistantDeltaResult {
    assistant_message_id: Option<MessageId>,
    assistant_message_created: bool,
}

impl AssistantDeltaResult {
    pub fn assistant_message_id(&self) -> Option<MessageId> {
        self.assistant_message_id
    }

    pub fn assistant_message_created(&self) -> bool {
        self.assistant_message_created
    }
}

/// The complete in-memory state of one process-lifetime conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    current_model_id: String,
    next_run_id: u64,
    next_message_id: u64,
    conversation_records: Vec<ConversationRecord>,
    runs: Vec<Run>,
}

impl Session {
    pub fn new(current_model_id: String) -> Self {
        Self {
            current_model_id,
            next_run_id: 1,
            next_message_id: 1,
            conversation_records: Vec::new(),
            runs: Vec::new(),
        }
    }

    pub fn current_model_id(&self) -> &str {
        &self.current_model_id
    }

    #[cfg(test)]
    pub fn conversation_records(&self) -> &[ConversationRecord] {
        &self.conversation_records
    }

    #[cfg(test)]
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    pub fn select_model(&mut self, model_id: String) {
        self.current_model_id = model_id;
    }

    /// Start a run and snapshot every currently model-visible record.
    pub fn start_run(&mut self, user_content: String) -> Result<RunStart, SessionError> {
        if let Some(run_id) = self.active_run_id() {
            return Err(SessionError::RunInProgress { run_id });
        }

        self.validate_model_visible_history()?;

        let (run_id, next_run_id) = self.next_run_id()?;
        let (message_id, next_message_id) = self.next_message_id()?;
        let user_message = ModelMessage::User {
            content: user_content,
        };
        let mut request_messages = self.model_visible_messages();
        request_messages.push(user_message.clone());
        let model_id = self.current_model_id.clone();
        let request = ModelRequest {
            model_id: model_id.clone(),
            messages: request_messages,
        };

        self.next_run_id = next_run_id;
        self.next_message_id = next_message_id;
        self.conversation_records.push(ConversationRecord {
            message_id,
            run_id,
            message: user_message,
            model_visible: true,
        });
        self.runs.push(Run {
            run_id,
            model_id: model_id.clone(),
            exchanges: vec![ProviderExchange {
                request: request.clone(),
                assistant_message_id: None,
                usage: None,
                terminal_state: RunTerminalState::Running,
            }],
            terminal_state: RunTerminalState::Running,
        });

        Ok(RunStart {
            run_id,
            user_message_id: message_id,
            request,
        })
    }

    /// Append a non-empty assistant text delta to the run's stable assistant record.
    pub fn append_assistant_delta(
        &mut self,
        run_id: RunId,
        text_delta: String,
    ) -> Result<AssistantDeltaResult, SessionError> {
        let run_index = self.running_run_index(run_id)?;
        if text_delta.is_empty() {
            return Ok(AssistantDeltaResult {
                assistant_message_id: None,
                assistant_message_created: false,
            });
        }

        self.ensure_assistant_output_can_continue(run_id)?;
        let assistant_message_id = self.exchange_for_run(run_index)?.assistant_message_id;

        if let Some(assistant_message_id) = assistant_message_id {
            let record_index = self
                .conversation_record_index(assistant_message_id)
                .ok_or_else(|| self.invalid_protocol("assistant record is missing"))?;
            let record = &mut self.conversation_records[record_index];
            match &mut record.message {
                ModelMessage::Assistant { content, .. } => content.push_str(&text_delta),
                _ => {
                    return Err(self.invalid_protocol("assistant identity points to another role"));
                }
            }
            return Ok(AssistantDeltaResult {
                assistant_message_id: Some(assistant_message_id),
                assistant_message_created: false,
            });
        }

        let (message_id, next_message_id) = self.next_message_id()?;
        self.next_message_id = next_message_id;
        self.conversation_records.push(ConversationRecord {
            message_id,
            run_id,
            message: ModelMessage::Assistant {
                content: text_delta,
                tool_calls: Vec::new(),
            },
            model_visible: false,
        });
        self.exchange_for_run_mut(run_index)?.assistant_message_id = Some(message_id);
        Ok(AssistantDeltaResult {
            assistant_message_id: Some(message_id),
            assistant_message_created: true,
        })
    }

    /// Append one complete provider tool call to the run's stable assistant record.
    #[allow(dead_code)]
    pub fn append_tool_call(
        &mut self,
        run_id: RunId,
        tool_call: ToolCall,
    ) -> Result<(), SessionError> {
        let run_index = self.running_run_index(run_id)?;
        if tool_call.tool_call_id.is_empty() {
            return Err(SessionError::EmptyToolCallId);
        }
        if self.tool_call_id_exists(&tool_call.tool_call_id) {
            return Err(SessionError::DuplicateToolCallId {
                tool_call_id: tool_call.tool_call_id,
            });
        }

        self.ensure_assistant_output_can_continue(run_id)?;
        let assistant_message_id = self.exchange_for_run(run_index)?.assistant_message_id;

        if let Some(assistant_message_id) = assistant_message_id {
            let record_index = self
                .conversation_record_index(assistant_message_id)
                .ok_or_else(|| self.invalid_protocol("assistant record is missing"))?;
            let record = &mut self.conversation_records[record_index];
            match &mut record.message {
                ModelMessage::Assistant { tool_calls, .. } => tool_calls.push(tool_call),
                _ => {
                    return Err(self.invalid_protocol("assistant identity points to another role"));
                }
            }
            return Ok(());
        }

        let (message_id, next_message_id) = self.next_message_id()?;
        self.next_message_id = next_message_id;
        self.conversation_records.push(ConversationRecord {
            message_id,
            run_id,
            message: ModelMessage::Assistant {
                content: String::new(),
                tool_calls: vec![tool_call],
            },
            model_visible: false,
        });
        self.exchange_for_run_mut(run_index)?.assistant_message_id = Some(message_id);
        Ok(())
    }

    /// Replace the current exchange's cumulative provider usage.
    pub fn record_usage(&mut self, run_id: RunId, usage: Usage) -> Result<(), SessionError> {
        let run_index = self.running_run_index(run_id)?;
        self.exchange_for_run_mut(run_index)?.usage = Some(usage);
        Ok(())
    }

    /// Append a correlated tool result without creating another exchange.
    #[allow(dead_code)]
    pub fn append_tool_result(
        &mut self,
        run_id: RunId,
        tool_call_id: String,
        content: String,
    ) -> Result<(), SessionError> {
        let run_index = self.running_run_index(run_id)?;
        if tool_call_id.is_empty() {
            return Err(SessionError::EmptyToolCallId);
        }

        let assistant_message_id = self.exchange_for_run(run_index)?.assistant_message_id;
        let Some(assistant_message_id) = assistant_message_id else {
            return Err(SessionError::MissingToolCall {
                run_id,
                tool_call_id,
            });
        };
        let assistant_record_index = self
            .conversation_record_index(assistant_message_id)
            .ok_or_else(|| self.invalid_protocol("assistant record is missing"))?;
        let tool_call_ids = match self.conversation_records[assistant_record_index].message() {
            ModelMessage::Assistant { tool_calls, .. } => tool_calls
                .iter()
                .map(|tool_call| tool_call.tool_call_id.clone())
                .collect::<Vec<_>>(),
            _ => return Err(self.invalid_protocol("assistant identity points to another role")),
        };
        if tool_call_ids.is_empty() {
            return Err(SessionError::MissingToolCall {
                run_id,
                tool_call_id,
            });
        }

        let resolved_tool_call_ids = self.resolved_tool_call_ids(run_id, assistant_record_index)?;
        if resolved_tool_call_ids
            .iter()
            .any(|resolved_tool_call_id| resolved_tool_call_id == &tool_call_id)
        {
            return Err(SessionError::ToolCallAlreadyResolved {
                run_id,
                tool_call_id,
            });
        }
        if !tool_call_ids
            .iter()
            .any(|declared_tool_call_id| declared_tool_call_id == &tool_call_id)
        {
            return Err(SessionError::MissingToolCall {
                run_id,
                tool_call_id,
            });
        }

        let expected_tool_call_id = tool_call_ids
            .iter()
            .find(|declared_tool_call_id| {
                !resolved_tool_call_ids
                    .iter()
                    .any(|resolved_tool_call_id| resolved_tool_call_id == *declared_tool_call_id)
            })
            .ok_or_else(|| self.invalid_protocol("all assistant tool calls are resolved"))?;
        if expected_tool_call_id != &tool_call_id {
            return Err(SessionError::ToolResultOutOfOrder {
                run_id,
                expected_tool_call_id: expected_tool_call_id.clone(),
                actual_tool_call_id: tool_call_id,
            });
        }

        let (message_id, next_message_id) = self.next_message_id()?;
        self.next_message_id = next_message_id;
        self.conversation_records.push(ConversationRecord {
            message_id,
            run_id,
            message: ModelMessage::ToolResult {
                tool_call_id,
                content,
            },
            model_visible: true,
        });
        Ok(())
    }

    /// Finish a run with the provider's normalized completion outcome.
    pub fn finish_run(
        &mut self,
        run_id: RunId,
        completion_outcome: CompletionOutcome,
    ) -> Result<(), SessionError> {
        let run_index = self.running_run_index(run_id)?;
        let exchange = self.exchange_for_run(run_index)?;
        let assistant_message_id = exchange.assistant_message_id;
        if let Some(assistant_message_id) = assistant_message_id {
            let assistant_record_index = self
                .conversation_record_index(assistant_message_id)
                .ok_or_else(|| self.invalid_protocol("assistant record is missing"))?;
            if let Some(tool_call_id) =
                self.unresolved_tool_call_id(run_id, assistant_record_index)?
            {
                return Err(SessionError::UnresolvedToolCall {
                    run_id,
                    tool_call_id,
                });
            }
        }

        if let Some(assistant_message_id) = assistant_message_id {
            let assistant_record_index = self
                .conversation_record_index(assistant_message_id)
                .ok_or_else(|| self.invalid_protocol("assistant record is missing"))?;
            self.conversation_records[assistant_record_index].model_visible = true;
        }
        self.set_terminal_state(run_index, RunTerminalState::Completed(completion_outcome))
    }

    /// Fail a run while retaining its request, partial response, usage, and detail.
    pub fn fail_run(
        &mut self,
        run_id: RunId,
        category: FailureCategory,
        display_detail: String,
    ) -> Result<(), SessionError> {
        let run_index = self.running_run_index(run_id)?;
        self.set_terminal_state(
            run_index,
            RunTerminalState::Failed(SessionFailure {
                category,
                display_detail,
            }),
        )
    }

    fn active_run_id(&self) -> Option<RunId> {
        self.runs
            .iter()
            .find(|run| matches!(run.terminal_state, RunTerminalState::Running))
            .map(Run::id)
    }

    fn running_run_index(&self, run_id: RunId) -> Result<usize, SessionError> {
        let run_index = self
            .runs
            .iter()
            .position(|run| run.run_id == run_id)
            .ok_or(SessionError::UnknownRun { run_id })?;
        if !matches!(
            self.runs[run_index].terminal_state,
            RunTerminalState::Running
        ) {
            return Err(SessionError::RunAlreadyTerminal { run_id });
        }
        Ok(run_index)
    }

    fn next_run_id(&self) -> Result<(RunId, u64), SessionError> {
        let next_counter = self
            .next_run_id
            .checked_add(1)
            .ok_or(SessionError::RunIdExhausted)?;
        Ok((RunId(self.next_run_id), next_counter))
    }

    fn next_message_id(&self) -> Result<(MessageId, u64), SessionError> {
        let next_counter = self
            .next_message_id
            .checked_add(1)
            .ok_or(SessionError::MessageIdExhausted)?;
        Ok((MessageId(self.next_message_id), next_counter))
    }

    fn model_visible_messages(&self) -> Vec<ModelMessage> {
        self.conversation_records
            .iter()
            .filter(|record| record.model_visible)
            .map(|record| record.message.clone())
            .collect()
    }

    fn validate_model_visible_history(&self) -> Result<(), SessionError> {
        let mut unresolved_tool_call_ids: Vec<String> = Vec::new();

        for record in self
            .conversation_records
            .iter()
            .filter(|record| record.model_visible)
        {
            match record.message() {
                ModelMessage::User { .. } => {
                    if let Some(tool_call_id) = unresolved_tool_call_ids.first() {
                        return Err(self.invalid_protocol(&format!(
                            "user record follows unresolved tool call {tool_call_id}"
                        )));
                    }
                }
                ModelMessage::Assistant { tool_calls, .. } => {
                    if let Some(tool_call_id) = unresolved_tool_call_ids.first() {
                        return Err(self.invalid_protocol(&format!(
                            "assistant record follows unresolved tool call {tool_call_id}"
                        )));
                    }
                    unresolved_tool_call_ids = tool_calls
                        .iter()
                        .map(|tool_call| tool_call.tool_call_id.clone())
                        .collect();
                }
                ModelMessage::ToolResult { tool_call_id, .. } => {
                    let Some(expected_tool_call_id) = unresolved_tool_call_ids.first() else {
                        return Err(self.invalid_protocol(
                            "tool result does not follow an assistant tool-call group",
                        ));
                    };
                    if expected_tool_call_id != tool_call_id {
                        return Err(self.invalid_protocol(&format!(
                            "tool result expected {expected_tool_call_id}, received {tool_call_id}"
                        )));
                    }
                    unresolved_tool_call_ids.remove(0);
                }
            }
        }

        if let Some(tool_call_id) = unresolved_tool_call_ids.first() {
            return Err(self.invalid_protocol(&format!(
                "conversation ends with unresolved tool call {tool_call_id}"
            )));
        }
        Ok(())
    }

    fn ensure_assistant_output_can_continue(&self, run_id: RunId) -> Result<(), SessionError> {
        if self.conversation_records.iter().any(|record| {
            record.run_id == run_id && matches!(record.message(), ModelMessage::ToolResult { .. })
        }) {
            return Err(
                self.invalid_protocol("assistant output cannot continue after a tool result")
            );
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn tool_call_id_exists(&self, tool_call_id: &str) -> bool {
        self.conversation_records.iter().any(|record| {
            let ModelMessage::Assistant { tool_calls, .. } = record.message() else {
                return false;
            };
            tool_calls
                .iter()
                .any(|tool_call| tool_call.tool_call_id == tool_call_id)
        })
    }

    fn resolved_tool_call_ids(
        &self,
        run_id: RunId,
        assistant_record_index: usize,
    ) -> Result<Vec<String>, SessionError> {
        let mut resolved_tool_call_ids = Vec::new();
        for record in self
            .conversation_records
            .iter()
            .skip(assistant_record_index + 1)
        {
            if record.run_id != run_id {
                continue;
            }
            let ModelMessage::ToolResult { tool_call_id, .. } = record.message() else {
                return Err(self.invalid_protocol(
                    "a non-tool-result record follows the assistant tool-call group",
                ));
            };
            resolved_tool_call_ids.push(tool_call_id.clone());
        }
        Ok(resolved_tool_call_ids)
    }

    fn unresolved_tool_call_id(
        &self,
        run_id: RunId,
        assistant_record_index: usize,
    ) -> Result<Option<String>, SessionError> {
        let tool_call_ids = match self.conversation_records[assistant_record_index].message() {
            ModelMessage::Assistant { tool_calls, .. } => tool_calls
                .iter()
                .map(|tool_call| tool_call.tool_call_id.clone())
                .collect::<Vec<_>>(),
            _ => return Err(self.invalid_protocol("assistant identity points to another role")),
        };
        let resolved_tool_call_ids = self.resolved_tool_call_ids(run_id, assistant_record_index)?;
        Ok(tool_call_ids
            .into_iter()
            .find(|tool_call_id| !resolved_tool_call_ids.contains(tool_call_id)))
    }

    fn conversation_record_index(&self, message_id: MessageId) -> Option<usize> {
        self.conversation_records
            .iter()
            .position(|record| record.message_id == message_id)
    }

    fn exchange_for_run(&self, run_index: usize) -> Result<&ProviderExchange, SessionError> {
        self.runs[run_index]
            .exchanges
            .first()
            .ok_or_else(|| self.invalid_protocol("run has no provider exchange"))
    }

    fn exchange_for_run_mut(
        &mut self,
        run_index: usize,
    ) -> Result<&mut ProviderExchange, SessionError> {
        let exchanges = &mut self.runs[run_index].exchanges;
        let Some(exchange) = exchanges.first_mut() else {
            return Err(SessionError::InvalidProtocol {
                detail: "run has no provider exchange".to_owned(),
            });
        };
        Ok(exchange)
    }

    fn set_terminal_state(
        &mut self,
        run_index: usize,
        terminal_state: RunTerminalState,
    ) -> Result<(), SessionError> {
        let run = &mut self.runs[run_index];
        let Some(exchange) = run.exchanges.first_mut() else {
            return Err(SessionError::InvalidProtocol {
                detail: "run has no provider exchange".to_owned(),
            });
        };
        exchange.terminal_state = terminal_state.clone();
        run.terminal_state = terminal_state;
        Ok(())
    }

    fn invalid_protocol(&self, detail: &str) -> SessionError {
        SessionError::InvalidProtocol {
            detail: detail.to_owned(),
        }
    }

    #[cfg(test)]
    fn set_next_counters_for_test(&mut self, next_run_id: u64, next_message_id: u64) {
        self.next_run_id = next_run_id;
        self.next_message_id = next_message_id;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CompletionOutcome, FailureCategory, ModelMessage, RunTerminalState, Session, SessionError,
        ToolCall, Usage,
    };

    const FIRST_MODEL_ID: &str = "model/first";
    const SECOND_MODEL_ID: &str = "model/second";

    fn usage(input_tokens: u64, output_tokens: u64) -> Usage {
        Usage {
            input_tokens,
            cached_input_tokens: input_tokens / 2,
            output_tokens,
        }
    }

    fn tool_call(tool_call_id: &str, tool_name: &str) -> ToolCall {
        ToolCall {
            tool_call_id: tool_call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            arguments: json!({"query": tool_call_id}),
        }
    }

    fn messages(session: &Session) -> Vec<ModelMessage> {
        session
            .conversation_records()
            .iter()
            .filter(|record| record.is_model_visible())
            .map(|record| record.message().clone())
            .collect()
    }

    fn all_records(session: &Session) -> Vec<(u64, u64, ModelMessage, bool)> {
        session
            .conversation_records()
            .iter()
            .map(|record| {
                (
                    record.message_id().as_u64(),
                    record.run_id().as_u64(),
                    record.message().clone(),
                    record.is_model_visible(),
                )
            })
            .collect()
    }

    fn start_run(session: &mut Session, content: &str) -> super::RunId {
        session
            .start_run(content.to_owned())
            .expect("run should start")
            .run_id()
    }

    #[test]
    fn two_runs_snapshot_ordered_history_and_preserve_model_and_usage() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let first_start = session
            .start_run("first request".to_owned())
            .expect("first run should start");
        let first_run_id = first_start.run_id();
        assert_eq!(first_start.user_message_id().as_u64(), 1);
        assert_eq!(first_start.request().model_id, FIRST_MODEL_ID);
        assert_eq!(
            first_start.request().messages,
            vec![ModelMessage::User {
                content: "first request".to_owned(),
            }]
        );
        let first_delta = session
            .append_assistant_delta(first_run_id, "first ".to_owned())
            .expect("first response delta should append");
        assert!(first_delta.assistant_message_created());
        let first_assistant_message_id = session.runs()[0].exchanges()[0]
            .assistant_message_id()
            .expect("first response should own an assistant record");
        assert_eq!(
            first_delta.assistant_message_id(),
            Some(first_assistant_message_id)
        );
        let later_delta = session
            .append_assistant_delta(first_run_id, "response".to_owned())
            .expect("second response delta should append");
        assert!(!later_delta.assistant_message_created());
        assert_eq!(
            later_delta.assistant_message_id(),
            Some(first_assistant_message_id)
        );
        assert_eq!(
            session.runs()[0].exchanges()[0].assistant_message_id(),
            Some(first_assistant_message_id)
        );
        let first_usage = usage(10, 4);
        session
            .record_usage(first_run_id, first_usage)
            .expect("first usage should append");
        session
            .finish_run(first_run_id, CompletionOutcome::Complete)
            .expect("first run should finish");

        session.select_model(SECOND_MODEL_ID.to_owned());
        let second_run_id = start_run(&mut session, "second request");
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(second_run_id.as_u64(), 2);
        assert_eq!(session.current_model_id(), SECOND_MODEL_ID);
        assert_eq!(session.runs()[0].model_id(), FIRST_MODEL_ID);
        assert_eq!(session.runs()[1].model_id(), SECOND_MODEL_ID);
        assert_eq!(
            session.runs()[0].exchanges()[0].request().model_id,
            FIRST_MODEL_ID
        );
        assert_eq!(
            session.runs()[1].exchanges()[0].request().model_id,
            SECOND_MODEL_ID
        );
        assert_eq!(session.runs()[0].exchanges()[0].usage(), Some(first_usage));
        assert_eq!(session.runs()[1].exchanges()[0].usage(), None);
        assert_eq!(
            session.runs()[1].exchanges()[0].request().messages,
            vec![
                ModelMessage::User {
                    content: "first request".to_owned(),
                },
                ModelMessage::Assistant {
                    content: "first response".to_owned(),
                    tool_calls: Vec::new(),
                },
                ModelMessage::User {
                    content: "second request".to_owned(),
                },
            ]
        );

        let second_usage = usage(20, 8);
        session
            .append_assistant_delta(second_run_id, "second response".to_owned())
            .expect("second response should append");
        session
            .record_usage(second_run_id, usage(19, 7))
            .expect("initial second usage should append");
        session
            .record_usage(second_run_id, second_usage)
            .expect("latest second usage should replace the initial usage");
        session
            .finish_run(second_run_id, CompletionOutcome::LengthLimited)
            .expect("second run should finish");

        assert_eq!(session.runs()[0].exchanges()[0].usage(), Some(first_usage));
        assert_eq!(session.runs()[1].exchanges()[0].usage(), Some(second_usage));
        assert_eq!(
            session.runs()[0].terminal_state(),
            &RunTerminalState::Completed(CompletionOutcome::Complete)
        );
        assert_eq!(
            session.runs()[0].exchanges()[0].terminal_state(),
            &RunTerminalState::Completed(CompletionOutcome::Complete)
        );
        assert_eq!(
            session.runs()[1].terminal_state(),
            &RunTerminalState::Completed(CompletionOutcome::LengthLimited)
        );
        assert_eq!(
            session.runs()[1].exchanges()[0].terminal_state(),
            &RunTerminalState::Completed(CompletionOutcome::LengthLimited)
        );
        assert_eq!(
            all_records(&session)
                .iter()
                .map(|record| (record.0, record.1))
                .collect::<Vec<_>>(),
            vec![(1, 1), (2, 1), (3, 2), (4, 2)]
        );
        assert!(all_records(&session).iter().all(|record| record.3));
    }

    #[test]
    fn successful_no_content_response_does_not_create_empty_assistant_record() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let run_id = start_run(&mut session, "request");
        assert_eq!(
            session.runs()[0].terminal_state(),
            &RunTerminalState::Running
        );
        assert_eq!(
            session.runs()[0].exchanges()[0].terminal_state(),
            &RunTerminalState::Running
        );
        session
            .append_assistant_delta(run_id, String::new())
            .expect("empty delta should be ignored");
        session
            .finish_run(run_id, CompletionOutcome::Complete)
            .expect("run should finish");

        assert_eq!(session.conversation_records().len(), 1);
        assert!(matches!(
            session.conversation_records()[0].message(),
            ModelMessage::User { content } if content == "request"
        ));
        assert_eq!(
            session.runs()[0].exchanges()[0].assistant_message_id(),
            None
        );
    }

    #[test]
    fn failed_runs_retain_requests_and_partial_data_but_exclude_assistant_history() {
        let mut setup_session = Session::new(FIRST_MODEL_ID.to_owned());
        let setup_run_id = start_run(&mut setup_session, "setup request");
        let setup_detail = "could not create request";
        setup_session
            .fail_run(
                setup_run_id,
                FailureCategory::RequestSetup,
                setup_detail.to_owned(),
            )
            .expect("setup failure should finish run");
        let RunTerminalState::Failed(setup_failure) = setup_session.runs()[0].terminal_state()
        else {
            panic!("setup run should be failed");
        };
        assert_eq!(setup_failure.category(), FailureCategory::RequestSetup);
        assert_eq!(setup_failure.display_detail(), setup_detail);
        assert_eq!(
            setup_session.runs()[0].exchanges()[0].terminal_state(),
            setup_session.runs()[0].terminal_state()
        );
        assert_eq!(
            messages(&setup_session),
            vec![ModelMessage::User {
                content: "setup request".to_owned(),
            }]
        );

        let mut streaming_session = Session::new(FIRST_MODEL_ID.to_owned());
        let streaming_run_id = start_run(&mut streaming_session, "streaming request");
        streaming_session
            .append_assistant_delta(streaming_run_id, "partial response".to_owned())
            .expect("partial response should append");
        let streaming_usage = usage(11, 3);
        streaming_session
            .record_usage(streaming_run_id, streaming_usage)
            .expect("streaming usage should append");
        streaming_session
            .fail_run(
                streaming_run_id,
                FailureCategory::Streaming,
                "provider stream failed".to_owned(),
            )
            .expect("streaming failure should finish run");
        assert_eq!(
            streaming_session.runs()[0].exchanges()[0].usage(),
            Some(streaming_usage)
        );
        assert_eq!(
            streaming_session.runs()[0].exchanges()[0].terminal_state(),
            &RunTerminalState::Failed(super::SessionFailure {
                category: FailureCategory::Streaming,
                display_detail: "provider stream failed".to_owned(),
            })
        );
        assert_eq!(
            streaming_session.runs()[0].exchanges()[0].terminal_state(),
            streaming_session.runs()[0].terminal_state()
        );
        assert!(matches!(
            streaming_session.conversation_records()[1].message(),
            ModelMessage::Assistant { content, .. } if content == "partial response"
        ));
        assert!(!streaming_session.conversation_records()[1].is_model_visible());
        assert_eq!(
            messages(&streaming_session),
            vec![ModelMessage::User {
                content: "streaming request".to_owned(),
            }]
        );

        let mut incomplete_session = Session::new(FIRST_MODEL_ID.to_owned());
        let incomplete_run_id = start_run(&mut incomplete_session, "incomplete request");
        incomplete_session
            .append_assistant_delta(incomplete_run_id, "incomplete response".to_owned())
            .expect("incomplete response should append");
        incomplete_session
            .fail_run(
                incomplete_run_id,
                FailureCategory::IncompleteStream,
                "stream closed without a terminal event".to_owned(),
            )
            .expect("incomplete failure should finish run");
        let RunTerminalState::Failed(incomplete_failure) =
            incomplete_session.runs()[0].terminal_state()
        else {
            panic!("incomplete run should be failed");
        };
        assert_eq!(
            incomplete_failure.category(),
            FailureCategory::IncompleteStream
        );
        assert_eq!(
            incomplete_failure.display_detail(),
            "stream closed without a terminal event"
        );
        assert_eq!(
            incomplete_session.runs()[0].exchanges()[0].terminal_state(),
            incomplete_session.runs()[0].terminal_state()
        );

        let next_run_id = start_run(&mut incomplete_session, "next request");
        assert_eq!(next_run_id.as_u64(), 2);
        assert_eq!(
            incomplete_session.runs()[1].exchanges()[0]
                .request()
                .messages,
            vec![
                ModelMessage::User {
                    content: "incomplete request".to_owned(),
                },
                ModelMessage::User {
                    content: "next request".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn terminal_and_invalid_transitions_leave_state_unchanged() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let run_id = start_run(&mut session, "request");
        let before_active_start = session.clone();
        assert_eq!(
            session.start_run("another request".to_owned()),
            Err(SessionError::RunInProgress { run_id })
        );
        assert_eq!(session, before_active_start);

        assert_eq!(
            session.append_assistant_delta(super::RunId(999), "ignored".to_owned()),
            Err(SessionError::UnknownRun {
                run_id: super::RunId(999)
            })
        );
        assert_eq!(session, before_active_start);

        session
            .finish_run(run_id, CompletionOutcome::Complete)
            .expect("run should finish");
        let before_terminal_mutations = session.clone();
        assert_eq!(
            session.append_assistant_delta(run_id, "ignored".to_owned()),
            Err(SessionError::RunAlreadyTerminal { run_id })
        );
        assert_eq!(
            session.record_usage(run_id, usage(1, 1)),
            Err(SessionError::RunAlreadyTerminal { run_id })
        );
        assert_eq!(
            session.finish_run(run_id, CompletionOutcome::LengthLimited),
            Err(SessionError::RunAlreadyTerminal { run_id })
        );
        assert_eq!(
            session.fail_run(run_id, FailureCategory::Streaming, "duplicate".to_owned()),
            Err(SessionError::RunAlreadyTerminal { run_id })
        );
        assert_eq!(session, before_terminal_mutations);
    }

    #[test]
    fn counter_exhaustion_is_typed_and_atomic() {
        let mut run_exhausted_session = Session::new(FIRST_MODEL_ID.to_owned());
        run_exhausted_session.set_next_counters_for_test(u64::MAX, 1);
        let run_exhausted_before = run_exhausted_session.clone();
        assert_eq!(
            run_exhausted_session.start_run("request".to_owned()),
            Err(SessionError::RunIdExhausted)
        );
        assert_eq!(run_exhausted_session, run_exhausted_before);

        let mut message_exhausted_session = Session::new(FIRST_MODEL_ID.to_owned());
        message_exhausted_session.set_next_counters_for_test(1, u64::MAX);
        let message_exhausted_before = message_exhausted_session.clone();
        assert_eq!(
            message_exhausted_session.start_run("request".to_owned()),
            Err(SessionError::MessageIdExhausted)
        );
        assert_eq!(message_exhausted_session, message_exhausted_before);
    }

    #[test]
    fn tool_calls_and_results_require_declared_contiguous_order() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let run_id = start_run(&mut session, "find information");
        session
            .append_tool_call(run_id, tool_call("call-1", "search"))
            .expect("first tool call should append");
        session
            .append_tool_call(run_id, tool_call("call-2", "open"))
            .expect("second tool call should append");

        let before_invalid_result = session.clone();
        assert_eq!(
            session.append_tool_result(run_id, "call-2".to_owned(), "out of order".to_owned()),
            Err(SessionError::ToolResultOutOfOrder {
                run_id,
                expected_tool_call_id: "call-1".to_owned(),
                actual_tool_call_id: "call-2".to_owned(),
            })
        );
        assert_eq!(session, before_invalid_result);

        assert_eq!(
            session.append_tool_result(run_id, "missing".to_owned(), "missing".to_owned()),
            Err(SessionError::MissingToolCall {
                run_id,
                tool_call_id: "missing".to_owned(),
            })
        );
        assert_eq!(session, before_invalid_result);
        assert_eq!(
            session.append_tool_result(run_id, String::new(), "empty".to_owned()),
            Err(SessionError::EmptyToolCallId)
        );
        assert_eq!(session, before_invalid_result);

        session
            .append_tool_result(run_id, "call-1".to_owned(), "first result".to_owned())
            .expect("first result should append");
        let before_output_after_result = session.clone();
        assert_eq!(
            session.append_assistant_delta(run_id, "late response".to_owned()),
            Err(SessionError::InvalidProtocol {
                detail: "assistant output cannot continue after a tool result".to_owned(),
            })
        );
        assert_eq!(
            session.append_tool_call(run_id, tool_call("call-3", "late")),
            Err(SessionError::InvalidProtocol {
                detail: "assistant output cannot continue after a tool result".to_owned(),
            })
        );
        assert_eq!(session, before_output_after_result);
        let before_duplicate_result = session.clone();
        assert_eq!(
            session.append_tool_result(run_id, "call-1".to_owned(), "duplicate".to_owned()),
            Err(SessionError::ToolCallAlreadyResolved {
                run_id,
                tool_call_id: "call-1".to_owned(),
            })
        );
        assert_eq!(session, before_duplicate_result);

        session
            .append_tool_result(run_id, "call-2".to_owned(), "second result".to_owned())
            .expect("second result should append");
        session
            .finish_run(run_id, CompletionOutcome::Complete)
            .expect("resolved tool calls should permit completion");
        assert_eq!(
            messages(&session),
            vec![
                ModelMessage::User {
                    content: "find information".to_owned(),
                },
                ModelMessage::Assistant {
                    content: String::new(),
                    tool_calls: vec![tool_call("call-1", "search"), tool_call("call-2", "open")],
                },
                ModelMessage::ToolResult {
                    tool_call_id: "call-1".to_owned(),
                    content: "first result".to_owned(),
                },
                ModelMessage::ToolResult {
                    tool_call_id: "call-2".to_owned(),
                    content: "second result".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn tool_call_ids_are_non_empty_and_unique_without_partial_mutation() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let run_id = start_run(&mut session, "request");
        let before_empty_call = session.clone();
        assert_eq!(
            session.append_tool_call(run_id, tool_call("", "search")),
            Err(SessionError::EmptyToolCallId)
        );
        assert_eq!(session, before_empty_call);

        session
            .append_tool_call(run_id, tool_call("call-1", "search"))
            .expect("first call should append");
        let before_duplicate_call = session.clone();
        assert_eq!(
            session.append_tool_call(run_id, tool_call("call-1", "again")),
            Err(SessionError::DuplicateToolCallId {
                tool_call_id: "call-1".to_owned(),
            })
        );
        assert_eq!(session, before_duplicate_call);

        session
            .fail_run(
                run_id,
                FailureCategory::IncompleteStream,
                "closed".to_owned(),
            )
            .expect("failed run should finish");
        let next_run_id = start_run(&mut session, "next request");
        let before_reused_call = session.clone();
        assert_eq!(
            session.append_tool_call(next_run_id, tool_call("call-1", "reuse")),
            Err(SessionError::DuplicateToolCallId {
                tool_call_id: "call-1".to_owned(),
            })
        );
        assert_eq!(session, before_reused_call);
    }

    #[test]
    fn unresolved_tool_calls_block_terminal_completion_without_mutation() {
        let mut session = Session::new(FIRST_MODEL_ID.to_owned());
        let run_id = start_run(&mut session, "request");
        session
            .append_tool_call(run_id, tool_call("call-1", "search"))
            .expect("tool call should append");
        let before_finish = session.clone();
        assert_eq!(
            session.finish_run(run_id, CompletionOutcome::Complete),
            Err(SessionError::UnresolvedToolCall {
                run_id,
                tool_call_id: "call-1".to_owned(),
            })
        );
        assert_eq!(session, before_finish);
    }
}
