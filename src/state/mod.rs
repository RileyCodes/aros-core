//! Engine state machine — manages mode transitions and dialogue context
//!
//! Modes:
//! - Listening: passive ASR, show subtitles + translation, trigger suggestions
//! - Dialogue: active conversation with Claude (serial agent loop)

use std::time::Instant;

/// Conversation message roles
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Active engine mode
#[derive(Debug, Clone)]
pub enum Mode {
    /// Passive: show ASR subtitles + translation, auto-suggest
    Listening,
    /// Active conversation with Claude (serial agent loop handles messages)
    Dialogue {
        /// Raw API messages (supports text, tool_calls, tool results, images)
        messages: Vec<serde_json::Value>,
        waiting_for_response: bool,
    },
    /// Gemini Live: bidirectional voice chat (audio routed in Kotlin, not through Rust)
    GeminiLive,
}

impl PartialEq for Mode {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Listening => "listening",
            Mode::Dialogue { .. } => "dialogue",
            Mode::GeminiLive => "gemini_live",
        }
    }

    pub fn is_dialogue(&self) -> bool {
        matches!(self, Mode::Dialogue { .. })
    }

    pub fn is_gemini_live(&self) -> bool {
        matches!(self, Mode::GeminiLive)
    }

    pub fn is_interactive(&self) -> bool {
        !matches!(self, Mode::Listening)
    }
}

/// State machine for the engine
pub struct StateMachine {
    pub mode: Mode,
    pub last_activity: Instant,
    dialogue_timeout_ms: u64,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            mode: Mode::Listening,
            last_activity: Instant::now(),
            dialogue_timeout_ms: 120_000, // 2 minutes of silence → exit dialogue
        }
    }

    /// Transition to dialogue mode
    pub fn enter_dialogue(&mut self) {
        self.mode = Mode::Dialogue {
            messages: Vec::new(),
            waiting_for_response: false,
        };
        self.last_activity = Instant::now();
    }

    /// Transition to Gemini Live mode
    pub fn enter_gemini_live(&mut self) {
        self.mode = Mode::GeminiLive;
        self.last_activity = Instant::now();
    }

    /// Return to listening mode
    pub fn exit_to_listening(&mut self) {
        self.mode = Mode::Listening;
        self.last_activity = Instant::now();
    }

    /// Add a user text message
    pub fn add_user_message(&mut self, text: &str) {
        self.last_activity = Instant::now();
        if let Mode::Dialogue {
            messages,
            waiting_for_response,
        } = &mut self.mode
        {
            messages.push(serde_json::json!({"role": "user", "content": text}));
            *waiting_for_response = true;
        }
        self.trim_messages();
    }

    /// Trim messages to stay under limit, preserving protocol-safe boundaries.
    /// Never split a tool_calls assistant message from its tool result messages.
    fn trim_messages(&mut self) {
        if let Mode::Dialogue { messages, .. } = &mut self.mode {
            while messages.len() > 20 {
                // Find the first safe cut point (don't cut inside tool_calls/tool pairs)
                let first_role = messages[0]["role"].as_str().unwrap_or("");
                if first_role == "tool" {
                    // Orphan tool result — remove it
                    messages.remove(0);
                } else if first_role == "assistant" && messages[0].get("tool_calls").is_some() {
                    // Assistant with tool_calls — remove it and all following tool results
                    messages.remove(0);
                    while !messages.is_empty() && messages[0]["role"].as_str() == Some("tool") {
                        messages.remove(0);
                    }
                } else {
                    messages.remove(0);
                }
            }
        }
    }

    /// Add an assistant text message
    pub fn add_assistant_message(&mut self, text: &str) {
        self.last_activity = Instant::now();
        if let Mode::Dialogue {
            messages,
            waiting_for_response,
        } = &mut self.mode
        {
            messages.push(serde_json::json!({"role": "assistant", "content": text}));
            *waiting_for_response = false;
        }
    }

    /// Add a raw assistant message (e.g., with tool_calls)
    pub fn add_raw_assistant_message(&mut self, msg: serde_json::Value) {
        self.last_activity = Instant::now();
        if let Mode::Dialogue { messages, .. } = &mut self.mode {
            messages.push(msg);
        }
    }

    /// Add a tool result message
    pub fn add_tool_result(&mut self, tool_call_id: &str, result: &str) {
        self.last_activity = Instant::now();
        if let Mode::Dialogue { messages, .. } = &mut self.mode {
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": result
            }));
        }
    }

    /// Add a user message with image content
    pub fn add_user_image(&mut self, base64_jpeg: &str, text: &str) {
        self.last_activity = Instant::now();
        if let Mode::Dialogue {
            messages,
            waiting_for_response,
        } = &mut self.mode
        {
            messages.push(serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", base64_jpeg)}},
                    {"type": "text", "text": text}
                ]
            }));
            *waiting_for_response = true;
        }
    }

    /// Get raw messages for API (includes system prompt placeholder)
    pub fn api_messages(&self) -> Vec<serde_json::Value> {
        match &self.mode {
            Mode::Dialogue { messages, .. } => messages.clone(),
            _ => Vec::new(),
        }
    }

    /// Legacy: get (Role, String) pairs for suggestion context etc.
    pub fn chat_history(&self) -> Vec<(Role, String)> {
        match &self.mode {
            Mode::Dialogue { messages, .. } => messages
                .iter()
                .filter_map(|m| {
                    let role = match m["role"].as_str()? {
                        "user" => Role::User,
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        "tool" => Role::Tool,
                        _ => return None,
                    };
                    let content = m["content"].as_str().unwrap_or("").to_string();
                    Some((role, content))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Check if dialogue has timed out
    pub fn check_timeout(&mut self) -> bool {
        if self.mode.is_interactive()
            && self.last_activity.elapsed().as_millis() > self.dialogue_timeout_ms as u128
        {
            self.exit_to_listening();
            return true;
        }
        false
    }

    /// Is the engine waiting for an AI response?
    pub fn is_waiting(&self) -> bool {
        match &self.mode {
            Mode::Dialogue {
                waiting_for_response,
                ..
            } => *waiting_for_response,
            _ => false,
        }
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let sm = StateMachine::new();
        assert_eq!(sm.mode, Mode::Listening);
        assert!(!sm.mode.is_interactive());
    }

    #[test]
    fn test_enter_dialogue() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();
        assert!(sm.mode.is_dialogue());
        assert!(sm.mode.is_interactive());
        assert_eq!(sm.mode.name(), "dialogue");
    }

    #[test]
    fn test_dialogue_flow() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();

        sm.add_user_message("大阪の天気は？");
        assert!(sm.is_waiting());
        assert_eq!(sm.chat_history().len(), 1);

        sm.add_assistant_message("大阪の今日の天気は晴れです。");
        assert!(!sm.is_waiting());
        assert_eq!(sm.chat_history().len(), 2);

        sm.add_user_message("明日は？");
        assert!(sm.is_waiting());
        assert_eq!(sm.chat_history().len(), 3);
    }

    #[test]
    fn test_exit_to_listening() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();
        sm.add_user_message("test");
        sm.exit_to_listening();
        assert_eq!(sm.mode, Mode::Listening);
        assert!(sm.chat_history().is_empty());
    }

    #[test]
    fn test_history_limit() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();
        for i in 0..25 {
            sm.add_user_message(&format!("msg {}", i));
        }
        assert_eq!(sm.chat_history().len(), 20);
        let history = sm.chat_history();
        assert!(history[0].1.contains("msg 5"));
    }

    #[test]
    fn test_timeout() {
        let mut sm = StateMachine::new();
        sm.dialogue_timeout_ms = 0;
        sm.enter_dialogue();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(sm.check_timeout());
        assert_eq!(sm.mode, Mode::Listening);
    }

    #[test]
    fn test_no_timeout_in_listening() {
        let mut sm = StateMachine::new();
        sm.dialogue_timeout_ms = 0;
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(!sm.check_timeout());
    }

    #[test]
    fn test_tool_messages() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();
        sm.add_user_message("Remember this");

        // Simulate tool call response
        sm.add_raw_assistant_message(serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "tc_1", "type": "function", "function": {"name": "update_memory", "arguments": "{}"}}]
        }));
        sm.add_tool_result("tc_1", "Done");

        let msgs = sm.api_messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "tc_1");
    }

    #[test]
    fn test_image_message() {
        let mut sm = StateMachine::new();
        sm.enter_dialogue();
        sm.add_user_image("abc123", "What is this?");

        let msgs = sm.api_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        // Content is array (image + text)
        assert!(msgs[0]["content"].is_array());
    }
}
