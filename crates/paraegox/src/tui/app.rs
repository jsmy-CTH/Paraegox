use std::collections::VecDeque;

use paraegox_agent::{SessionId, TurnTerminal};

pub(super) const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_UI_MESSAGES: usize = 64;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_NOTICE_BYTES: usize = 512;
const MAX_ENDPOINT_BYTES: usize = 256;

#[derive(Clone, Copy)]
pub(super) enum MessageRole {
    User,
    Agent,
    System,
}

pub(super) struct ChatMessage {
    pub(super) role: MessageRole,
    pub(super) content: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum AgentState {
    Idle,
    Waiting,
    Error,
}

pub(super) struct ChatApp {
    pub(super) target: String,
    pub(super) endpoint: String,
    pub(super) session_id: SessionId,
    pub(super) messages: VecDeque<ChatMessage>,
    pub(super) editor: InputEditor,
    pub(super) agent_state: AgentState,
    pub(super) submitted_turns: u64,
    pub(super) notice: String,
}

impl ChatApp {
    pub(super) fn new(target: String, endpoint: String, session_id: SessionId) -> Self {
        Self {
            target: bounded_display_text(&target, 64),
            endpoint: bounded_display_text(&endpoint, MAX_ENDPOINT_BYTES),
            session_id,
            messages: VecDeque::new(),
            editor: InputEditor::default(),
            agent_state: AgentState::Idle,
            submitted_turns: 0,
            notice: "Fabric session open; Agent availability is confirmed by a reply".to_owned(),
        }
    }

    pub(super) fn begin_turn(&mut self, input: &str) {
        self.push_message(MessageRole::User, input);
        self.agent_state = AgentState::Waiting;
        self.submitted_turns = self.submitted_turns.saturating_add(1);
        self.set_notice("Waiting for the Agent response");
    }

    pub(super) fn finish_turn(&mut self, terminal: TurnTerminal) {
        match terminal {
            TurnTerminal::Final { content } => {
                self.push_message(MessageRole::Agent, &content);
                self.agent_state = AgentState::Idle;
                self.set_notice("Agent reply received");
            }
            TurnTerminal::Cancelled => {
                self.push_message(MessageRole::System, "Turn cancelled");
                self.agent_state = AgentState::Idle;
                self.set_notice("The active turn was cancelled");
            }
            TurnTerminal::TimedOut => {
                self.push_message(MessageRole::System, "Turn timed out");
                self.agent_state = AgentState::Error;
                self.set_notice("The last turn timed out");
            }
            TurnTerminal::Failed { reason } => {
                self.push_message(MessageRole::System, &format!("Turn failed: {reason}"));
                self.agent_state = AgentState::Error;
                self.set_notice("The last turn failed");
            }
        }
    }

    pub(super) fn cancellation_requested(&mut self) {
        self.push_message(MessageRole::System, "Cancellation requested");
        self.set_notice("The active turn cancellation was accepted");
    }

    pub(super) fn push_message(&mut self, role: MessageRole, content: &str) {
        if self.messages.len() == MAX_UI_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(ChatMessage {
            role,
            content: bounded_display_text(content, MAX_MESSAGE_BYTES),
        });
    }

    pub(super) fn set_notice(&mut self, notice: &str) {
        self.notice = bounded_display_text(notice, MAX_NOTICE_BYTES);
    }
}

#[derive(Default)]
pub(super) struct InputEditor {
    pub(super) text: String,
    pub(super) cursor: usize,
}

impl InputEditor {
    pub(super) fn insert(&mut self, character: char) -> bool {
        if character.is_control() || self.text.len() + character.len_utf8() > MAX_INPUT_BYTES {
            return false;
        }
        let index = self.byte_index(self.cursor);
        self.text.insert(index, character);
        self.cursor += 1;
        true
    }

    pub(super) fn insert_text(&mut self, text: &str) -> bool {
        let mut complete = true;
        for character in text.chars().filter(|character| !character.is_control()) {
            if !self.insert(character) {
                complete = false;
                break;
            }
        }
        complete
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub(super) fn delete(&mut self) {
        if self.cursor == self.char_count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.char_count();
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub(super) fn prefix(&self) -> &str {
        &self.text[..self.byte_index(self.cursor)]
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_index(&self, character_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(character_index)
            .map_or(self.text.len(), |(index, _)| index)
    }
}

fn bounded_display_text(value: &str, max_bytes: usize) -> String {
    let mut output = String::with_capacity(value.len().min(max_bytes));
    let mut truncated = false;
    for character in value.chars() {
        let character = if character == '\n' {
            character
        } else if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if output.len() + character.len_utf8() > max_bytes.saturating_sub(3) {
            truncated = true;
            break;
        }
        output.push(character);
    }
    if truncated {
        output.push('…');
    }
    output
}
