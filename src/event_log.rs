//! JSONL event logger — structured logs for analysis
//!
//! Writes one JSON object per line to a .jsonl file.
//! Events: asr_partial, asr_final, sentence_end, translation,
//! dialogue_start, dialogue_end, tool_call, suggestion, error

use std::io::Write;
use std::sync::Mutex;

pub struct EventLogger {
    writer: Mutex<Option<std::io::BufWriter<std::fs::File>>>,
}

impl EventLogger {
    pub fn new() -> Self {
        Self {
            writer: Mutex::new(None),
        }
    }

    /// Start logging to a file. Call once at engine start.
    pub fn start(&self, path: &str) {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            *self.writer.lock().unwrap() = Some(std::io::BufWriter::new(file));
            log::info!("EventLogger: writing to {}", path);
        }
    }

    /// Log a structured event
    pub fn log(&self, event_type: &str, data: serde_json::Value) {
        let mut guard = self.writer.lock().unwrap();
        if let Some(ref mut w) = *guard {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let entry = serde_json::json!({
                "ts": ts,
                "type": event_type,
                "data": data
            });
            let _ = writeln!(w, "{}", entry);
            let _ = w.flush();
        }
    }
}

impl Default for EventLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for EventLogger {
    fn clone(&self) -> Self {
        // EventLogger is shared via Arc, clone just creates a new empty one
        // (actual sharing is through Arc in Engine)
        Self::new()
    }
}
