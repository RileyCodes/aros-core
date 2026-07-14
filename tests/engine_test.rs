//! Integration test for Engine initialization and basic flow
//! Runs on host (not Android) — tests core logic without JNI

use aros_core::*;
use std::sync::{Arc, Mutex as StdMutex};

/// Mock display callback that records all calls
struct MockDisplay {
    asr_calls: StdMutex<Vec<(String, i32, bool)>>,
    translation_calls: StdMutex<Vec<String>>,
    ai_response_calls: StdMutex<Vec<String>>,
    status_calls: StdMutex<Vec<String>>,
    mode_calls: StdMutex<Vec<String>>,
    speak_calls: StdMutex<Vec<(String, String)>>,
    memory_calls: StdMutex<Vec<String>>,
    error_calls: StdMutex<Vec<String>>,
}

impl MockDisplay {
    fn new() -> Self {
        Self {
            asr_calls: StdMutex::new(Vec::new()),
            translation_calls: StdMutex::new(Vec::new()),
            ai_response_calls: StdMutex::new(Vec::new()),
            status_calls: StdMutex::new(Vec::new()),
            mode_calls: StdMutex::new(Vec::new()),
            speak_calls: StdMutex::new(Vec::new()),
            memory_calls: StdMutex::new(Vec::new()),
            error_calls: StdMutex::new(Vec::new()),
        }
    }
}

impl DisplayCallback for MockDisplay {
    fn show_asr(&self, text: &str, speaker: i32, is_final: bool) {
        self.asr_calls
            .lock()
            .unwrap()
            .push((text.to_string(), speaker, is_final));
    }
    fn show_translation(&self, text: &str) {
        self.translation_calls
            .lock()
            .unwrap()
            .push(text.to_string());
    }
    fn show_suggestions(&self, _items: Vec<ai::Suggestion>) {}
    fn show_status(&self, status: &str) {
        self.status_calls.lock().unwrap().push(status.to_string());
    }
    fn show_mode(&self, mode: &str) {
        self.mode_calls.lock().unwrap().push(mode.to_string());
    }
    fn show_ai_response(&self, text: &str) {
        self.ai_response_calls
            .lock()
            .unwrap()
            .push(text.to_string());
    }
    fn speak(&self, text: &str, lang: &str) {
        self.speak_calls
            .lock()
            .unwrap()
            .push((text.to_string(), lang.to_string()));
    }
    fn take_photo(&self) {}
    fn on_memory_updated(&self, memory: &str) {
        self.memory_calls.lock().unwrap().push(memory.to_string());
    }
    fn on_error(&self, msg: &str) {
        self.error_calls.lock().unwrap().push(msg.to_string());
    }
}

#[test]
fn test_engine_creation() {
    let mock = Arc::new(MockDisplay::new());
    let cfg = config::Config::default();
    let (engine, _event_rx, _agent_rx) = Engine::new(mock.clone(), cfg);

    // Engine should have default config
    assert!(engine.config.suggest_enabled);
    assert_eq!(engine.config.locale.asr_language, "ja");

    // Memory should be empty
    assert!(engine.memory.is_empty());

    // Tool registry should have 2 tools
    assert_eq!(engine.tool_registry.len(), 4);
    assert!(engine.tool_registry.has("update_memory"));
    assert!(engine.tool_registry.has("take_photo"));
    assert!(engine.tool_registry.has("set_user_speaker"));
}

#[test]
fn test_memory_operations() {
    let mock = Arc::new(MockDisplay::new());
    let cfg = config::Config::default();
    let (engine, _event_rx, _agent_rx) = Engine::new(mock.clone(), cfg);

    assert!(engine.memory.is_empty());

    engine.memory.set("test memo");
    assert_eq!(engine.memory.get(), "test memo");

    engine.memory.set("line1\nline2");
    assert_eq!(engine.memory.get(), "line1\nline2");

    engine.memory.clear();
    assert!(engine.memory.is_empty());
}

#[test]
fn test_notification_display() {
    let mock = Arc::new(MockDisplay::new());
    let cfg = config::Config::default();
    let (engine, _event_rx, _agent_rx) = Engine::new(mock.clone(), cfg);

    engine.on_notification("LINE", "Riley", "Hello!");

    let asr = mock.asr_calls.lock().unwrap();
    assert_eq!(asr.len(), 1);
    assert!(asr[0].0.contains("LINE"));
    assert!(asr[0].0.contains("Riley"));
}

#[tokio::test]
async fn test_engine_start() {
    let mock = Arc::new(MockDisplay::new());
    let cfg = config::Config::default();
    let (engine, _event_rx, _agent_rx) = Engine::new(mock.clone(), cfg);

    engine.start().await;

    // Should have shown starting status
    let statuses = mock.status_calls.lock().unwrap();
    assert!(!statuses.is_empty());
}

#[test]
fn test_command_detection_integration() {
    let mock = Arc::new(MockDisplay::new());
    let cfg = config::Config::default();
    let (engine, _event_rx, _agent_rx) = Engine::new(mock.clone(), cfg);

    // Test that command parser requires 克劳德 prefix
    assert_eq!(
        engine.cmd.detect("クロード、天気は？"),
        Some(cmd::Command::EnterDialogue)
    );
    assert_eq!(
        engine.cmd.detect("克劳德状态"),
        Some(cmd::Command::ShowStatus)
    );
    // Without prefix, nothing triggers
    assert_eq!(engine.cmd.detect("ステータス"), None);
    assert_eq!(engine.cmd.detect("普通の文章です"), None);
}
