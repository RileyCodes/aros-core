//! ArOS Core — Cross-platform AR glasses engine
//!
//! Handles ASR, translation, AI suggestions, memory, and voice commands.
//! Platform-agnostic: the UI layer (Kotlin/Swift/terminal) implements DisplayCallback.

pub mod ai;
pub mod asr;
pub mod ble;
pub mod cmd;
pub mod event_log;
pub mod i18n;
pub mod memory;
pub mod net;
pub mod state;
pub mod tools;
pub mod translate;
pub mod uniffi_bridge;

uniffi::include_scaffolding!("uniffi_interface");
pub mod audio;
pub mod config;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Mutex;

/// Platform UI callback — implemented by Kotlin (Android) or other frontends.
///
/// All methods may be called from any thread. Implementors must handle
/// thread safety (e.g., `runOnUiThread` on Android).
pub trait DisplayCallback: Send + Sync {
    /// Show ASR transcription text. `speaker` is -1 for unknown, 0+ for identified speakers.
    fn show_asr(&self, text: &str, speaker: i32, is_final: bool);
    /// Show Soniox translation (ja↔zh bidirectional).
    fn show_translation(&self, text: &str);
    /// Show AI conversation suggestions (4 items with optional Chinese translation).
    fn show_suggestions(&self, items: Vec<ai::Suggestion>);
    /// Show status text (ping stats, connection state, errors).
    fn show_status(&self, status: &str);
    /// Notify UI of mode change: "listening" or "dialogue".
    fn show_mode(&self, mode: &str);
    /// Show Claude's response (distinct color from translation).
    fn show_ai_response(&self, text: &str);
    /// Request TTS playback. `lang`: "ja", "zh", "en", "ko".
    fn speak(&self, text: &str, lang: &str);
    /// Request camera capture. Result returned via `Engine::on_photo_taken`.
    fn take_photo(&self);
    /// Memory was updated (persist to SharedPreferences).
    fn on_memory_updated(&self, memory: &str);
    /// Error occurred in engine.
    fn on_error(&self, msg: &str);
}

/// Tracks recently spoken TTS text to filter ASR echo
struct TtsEchoGuard {
    /// Recently spoken texts with expiry timestamps
    spoken: Vec<(String, std::time::Instant)>,
    /// How long to suppress echo after TTS (covers TTS duration + trailing echo)
    suppress_duration: std::time::Duration,
}

impl TtsEchoGuard {
    fn new() -> Self {
        Self {
            spoken: Vec::new(),
            suppress_duration: std::time::Duration::from_secs(8), // generous window
        }
    }

    fn normalize(text: &str) -> String {
        text.chars()
            .filter(|c| {
                !c.is_whitespace()
                    && *c != '。'
                    && *c != '、'
                    && *c != '！'
                    && *c != '？'
                    && *c != '.'
                    && *c != ','
                    && *c != '!'
                    && *c != '?'
                    && (*c < '\u{1F000}') // strip emoji and symbols above U+1F000
            })
            .collect()
    }

    /// Record that TTS is about to speak this text
    fn record_spoken(&mut self, text: &str) {
        // Clean expired entries
        let now = std::time::Instant::now();
        self.spoken
            .retain(|(_, t)| now.duration_since(*t) < self.suppress_duration);
        // Cap at 20 entries to prevent unbounded growth
        while self.spoken.len() > 20 {
            self.spoken.remove(0);
        }
        let normalized = Self::normalize(text);
        if !normalized.is_empty() {
            self.spoken.push((normalized, now));
        }
    }

    /// Check if ASR text is likely an echo of recent TTS
    fn is_echo(&self, asr_text: &str) -> bool {
        let now = std::time::Instant::now();
        let normalized = Self::normalize(asr_text);
        if normalized.is_empty() {
            return false;
        }
        for (spoken, timestamp) in &self.spoken {
            if now.duration_since(*timestamp) > self.suppress_duration {
                continue;
            }
            // Check if ASR text is a substring of spoken text or vice versa
            // (ASR may capture partial TTS output)
            if spoken.contains(&normalized) || normalized.contains(spoken.as_str()) {
                log::info!(
                    "Echo suppressed: ASR=\"{}\" matches TTS=\"{}\"",
                    asr_text,
                    spoken
                );
                return true;
            }
            // Fuzzy: check if >60% of characters overlap
            let overlap = normalized.chars().filter(|c| spoken.contains(*c)).count();
            let ratio = overlap as f32 / normalized.len().max(1) as f32;
            if ratio > 0.6 && normalized.len() > 3 {
                log::info!(
                    "Echo suppressed (fuzzy {:.0}%): ASR=\"{}\"",
                    ratio * 100.0,
                    asr_text
                );
                return true;
            }
        }
        false
    }
}

/// Messages sent to the serial agent loop
pub enum AgentMessage {
    /// User spoke in dialogue mode
    UserText(String),
    /// Photo captured (base64 JPEG)
    Photo(String),
}

/// Agent state — lives inside the agent loop task, processes messages serially
pub struct AgentState {
    api_key: String,
    client: reqwest::Client,
    callback: Arc<dyn DisplayCallback>,
    echo_guard: Arc<Mutex<TtsEchoGuard>>,
    memory: memory::MemoryManager,
    tools_json: serde_json::Value,
    tts_enabled: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<Mutex<state::StateMachine>>,
    last_response: Arc<Mutex<String>>,
    user_speaker: Arc<std::sync::atomic::AtomicI32>,
    conversation_history: Arc<std::sync::RwLock<Vec<(i32, String)>>>,
    guide: memory::GuideMode,
}

fn speaker_label(spk: i32, user_spk: i32) -> String {
    if spk < 0 {
        "P?".to_string()
    } else {
        let base = format!("P{}", spk + 1);
        if spk == user_spk {
            format!("{}(You)", base)
        } else {
            base
        }
    }
}

// ASR latency tracking (module-level for access from both handle_asr_event and ping callback)
static LAST_SENTENCE_END: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
// Translation latency: time from Final to SentenceEnd (how long we waited for translation)
static TRANS_LATENCY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_FINAL_TIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const SUGGESTION_COOLDOWN_MS: u64 = 8_000;

pub struct Engine {
    pub asr: asr::AsrEngine,
    pub ai: ai::AiEngine,
    pub memory: memory::MemoryManager,
    pub guide: memory::GuideMode,
    pub event_log: std::sync::Arc<event_log::EventLogger>,
    pub cmd: cmd::CommandParser,
    pub config: config::Config,
    pub ping: net::PingMonitor,
    pub msg: i18n::Messages,
    pub state: Arc<Mutex<state::StateMachine>>,
    pub tool_registry: tools::ToolRegistry,
    start_time: std::time::Instant,
    callback: Arc<dyn DisplayCallback>,
    running: Arc<Mutex<bool>>,
    last_response: Arc<Mutex<String>>,
    tts_enabled: Arc<std::sync::atomic::AtomicBool>,
    tts_echo_guard: Arc<Mutex<TtsEchoGuard>>,
    /// Rolling conversation history: (speaker_id, text) pairs
    conversation_history: Arc<std::sync::RwLock<Vec<(i32, String)>>>,
    /// Which speaker ID is the user (-1 = unknown)
    user_speaker: Arc<std::sync::atomic::AtomicI32>,
    /// Channel to send messages to the serial agent loop
    agent_tx: tokio::sync::mpsc::Sender<AgentMessage>,
    last_suggest_ms: Arc<AtomicU64>,
    suggest_inflight: Arc<AtomicBool>,
}

impl Engine {
    pub fn new(
        callback: Arc<dyn DisplayCallback>,
        cfg: config::Config,
    ) -> (
        Self,
        tokio::sync::mpsc::Receiver<asr::AsrEvent>,
        tokio::sync::mpsc::Receiver<AgentMessage>,
    ) {
        let msg = i18n::Messages::new(&cfg.locale.ui_language);
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel::<AgentMessage>(8);

        // Register tools
        let mut tool_registry = tools::ToolRegistry::new();
        tool_registry.register(
            "update_memory",
            "Add, update, or delete items from the user's persistent memory/notes. Use when the user asks you to remember, forget, or change something.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "delete", "replace_all"]},
                    "content": {"type": "string", "description": "Content to add/delete/replace"}
                },
                "required": ["action", "content"]
            }),
            |_args| tools::ToolResult { success: true, message: "handled in engine".to_string() },
        );
        tool_registry.register(
            "take_photo",
            "Take a photo with the AR glasses camera. Use when the user asks to see, identify, or photograph something.",
            serde_json::json!({"type": "object", "properties": {}}),
            |_args| tools::ToolResult { success: true, message: "handled in engine".to_string() },
        );
        tool_registry.register(
            "set_user_speaker",
            "Set which speaker ID is the AR glasses wearer (user). Use when the user identifies themselves in the conversation, e.g. 'I'm the one talking about food'. Look at the conversation history speaker IDs to find the matching one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "speaker_id": {"type": "integer", "description": "The speaker index (0, 1, 2...) that is the user"}
                },
                "required": ["speaker_id"]
            }),
            |_args| tools::ToolResult { success: true, message: "handled in engine".to_string() },
        );
        tool_registry.register(
            "set_guide_mode",
            "Activate or deactivate guide mode. Guide mode provides task-specific context to help the user with a specific goal (e.g., making a phone call, clinic visit, shopping). Use when the user asks to start/stop guide mode or describes a task they need help with.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "active": {"type": "boolean", "description": "true to activate, false to deactivate"},
                    "label": {"type": "string", "description": "Short emoji+label for status bar, e.g. '📞 電話', '🏥 診察'"},
                    "goal": {"type": "string", "description": "What the user wants to achieve"},
                    "context": {"type": "string", "description": "Relevant context, expected Q&A, key phrases"}
                },
                "required": ["active"]
            }),
            |_args| tools::ToolResult { success: true, message: "handled in engine".to_string() },
        );

        let (asr, event_rx) = asr::AsrEngine::with_languages(
            &cfg.soniox_key,
            &cfg.locale.asr_language,
            &cfg.locale.translate_from,
            &cfg.locale.translate_to,
        );

        let engine = Self {
            asr,
            ai: ai::AiEngine::new(&cfg.openrouter_key),
            memory: memory::MemoryManager::new(),
            guide: memory::GuideMode::new(),
            event_log: std::sync::Arc::new(event_log::EventLogger::new()),
            cmd: cmd::CommandParser::new(),
            ping: net::PingMonitor::new(),
            msg,
            state: Arc::new(Mutex::new(state::StateMachine::new())),
            tool_registry,
            start_time: std::time::Instant::now(),
            config: cfg,
            callback,
            running: Arc::new(Mutex::new(false)),
            last_response: Arc::new(Mutex::new(String::new())),
            tts_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            tts_echo_guard: Arc::new(Mutex::new(TtsEchoGuard::new())),
            conversation_history: Arc::new(std::sync::RwLock::new(Vec::new())),
            user_speaker: Arc::new(std::sync::atomic::AtomicI32::new(-1)),
            agent_tx: agent_tx,
            last_suggest_ms: Arc::new(AtomicU64::new(0)),
            suggest_inflight: Arc::new(AtomicBool::new(false)),
        };
        (engine, event_rx, agent_rx)
    }

    /// Start the engine — call from UI thread, runs async internally
    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        if *running {
            return;
        }
        *running = true;
        drop(running);

        log::info!("Engine starting");
        // Start JSONL event logger (Android external files dir)
        self.event_log
            .start("/sdcard/Android/data/com.inmo.asr/files/events.jsonl");
        self.callback.show_status(self.msg.get("engine.starting"));

        // Start ping monitor (shows network + translation latency in status bar)
        let ping_cb = self.callback.clone();
        let ping = self.ping.clone();
        tokio::spawn(async move {
            ping.run(move |status| {
                // Show translation latency (time waiting for translation after Final)
                let trans_ms = TRANS_LATENCY_MS.load(std::sync::atomic::Ordering::Relaxed);
                if trans_ms > 0 && trans_ms < 10000 {
                    ping_cb.show_status(&format!("{} T:{}ms", status, trans_ms));
                } else {
                    ping_cb.show_status(status);
                }
            })
            .await;
        });

        // Dialogue/Gemini timeout must fire even when the user is silent.
        let timeout_state = self.state.clone();
        let timeout_cb = self.callback.clone();
        let timeout_started_msg = self.msg.get("engine.started").to_string();
        tokio::spawn(async move {
            let mut warned = false;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let action = {
                    let mut sm = timeout_state.lock().await;
                    if sm.mode.is_interactive() {
                        let elapsed = sm.last_activity.elapsed();
                        if sm.check_timeout() {
                            warned = false;
                            Some("timeout")
                        } else if elapsed.as_secs() > 90 && !warned {
                            warned = true;
                            Some("warn")
                        } else {
                            if elapsed.as_secs() <= 90 {
                                warned = false;
                            }
                            None
                        }
                    } else {
                        warned = false;
                        None
                    }
                };
                match action {
                    Some("timeout") => {
                        log::info!("Mode timed out, returning to listening");
                        timeout_cb.show_mode("listening");
                        timeout_cb.show_status(&timeout_started_msg);
                        timeout_cb.show_ai_response("");
                    }
                    Some("warn") => timeout_cb.show_status("💬 30s until timeout..."),
                    _ => {}
                }
            }
        });

        // Pre-connect ASR WebSocket in background (TLS handshake is slow on Android)
        let asr = self.asr.clone();
        let cb = self.callback.clone();
        let msg_connecting = self.msg.get("asr.connecting").to_string();
        let msg_connected = self.msg.get("asr.connected").to_string();
        let msg_failed = self.msg.get("asr.connect_failed").to_string();
        tokio::spawn(async move {
            cb.show_status(&msg_connecting);
            match asr.connect().await {
                Ok(_) => {
                    log::info!("ASR: pre-connected");
                    cb.show_status(&msg_connected);
                }
                Err(e) => {
                    log::error!("ASR: pre-connect failed: {}", e);
                    let lower = e.to_string().to_ascii_lowercase();
                    if lower.contains("timeout")
                        || lower.contains("lookup")
                        || lower.contains("resolve")
                        || lower.contains("network")
                        || lower.contains("host")
                    {
                        cb.show_status("NET BAD");
                    } else {
                        cb.show_status(&msg_failed);
                    }
                }
            }
        });

        log::info!("Engine started");
    }

    /// Feed raw PCM audio from the platform's microphone
    pub async fn feed_audio(&self, pcm: &[i16]) {
        self.asr.feed(pcm).await;
    }

    /// Get conversation history formatted with speaker labels
    pub fn conversation_history(&self) -> Vec<String> {
        let user_spk = self.user_speaker.load(std::sync::atomic::Ordering::Relaxed);
        self.conversation_history
            .read()
            .map(|entries| {
                entries
                    .iter()
                    .map(|(spk, text)| {
                        let label = speaker_label(*spk, user_spk);
                        format!("{}: {}", label, text)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Notify UI of guide mode status change
    pub fn show_guide_status(&self) {
        let label = self.guide.label();
        self.callback.show_status(&format!("GUIDE:{}", label));
    }

    /// Clear conversation history
    pub fn clear_history(&self) {
        if let Ok(mut h) = self.conversation_history.write() {
            h.clear();
        }
    }

    /// Handle an incoming notification from the platform
    pub fn on_notification(&self, app_name: &str, title: &str, body: &str) {
        let display = if body.is_empty() {
            format!("{}: {}", app_name, title)
        } else if title == body {
            format!("{}: {}", app_name, body)
        } else {
            format!("{}: {}", app_name, title)
        };
        log::info!("Notification: {} | {}", display, body);
        self.callback.show_asr(&display, -1, true);
        if !body.is_empty() && body != title {
            self.callback.show_translation(body);
        }
    }

    /// Called from Kotlin when photo is captured — sends to agent loop
    pub async fn on_photo_taken(&self, base64_jpeg: &str) {
        log::info!(
            "Photo received: {} bytes base64 ({}KB JPEG)",
            base64_jpeg.len(),
            base64_jpeg.len() * 3 / 4 / 1024
        );
        self.callback.show_status("📷 analyzing...");
        let _ = self
            .agent_tx
            .send(AgentMessage::Photo(base64_jpeg.to_string()))
            .await;
    }

    /// Handle ASR event from the event-driven pipeline
    pub async fn handle_asr_event(&self, event: asr::AsrEvent) {
        match event {
            asr::AsrEvent::Partial { text, speaker, .. } => {
                // Clear sentence-end timestamp so we don't re-measure
                let last_end = LAST_SENTENCE_END.load(std::sync::atomic::Ordering::Relaxed);
                if last_end > 0 {
                    LAST_SENTENCE_END.store(0, std::sync::atomic::Ordering::Relaxed);
                }
                log::info!(
                    "ASR partial: \"{}\"",
                    &text.chars().take(40).collect::<String>()
                );
                self.callback.show_asr(&text, speaker, false);
            }
            asr::AsrEvent::Final {
                utterance_id,
                text,
                translation,
                speaker,
                reason,
            } => {
                // Record time of Final for translation latency measurement
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                LAST_FINAL_TIME.store(now, std::sync::atomic::Ordering::Relaxed);

                self.event_log.log(
                    "asr_final",
                    serde_json::json!({
                        "utteranceId": utterance_id,
                        "text": &text,
                        "textChars": text.chars().count(),
                        "speaker": speaker,
                        "reason": reason,
                        "hasInitialTranslation": translation.as_ref().is_some_and(|s| !s.trim().is_empty()),
                        "translationChars": translation.as_ref().map(|s| s.chars().count()).unwrap_or(0)
                    }),
                );
                log::info!(
                    "ASR final: \"{}\" trans={}",
                    &text.chars().take(40).collect::<String>(),
                    translation.as_deref().unwrap_or("pending")
                );
                self.callback.show_asr(&text, speaker, true);
                // Only show translation in listening mode (dialogue mode doesn't need it)
                let is_listening = {
                    let sm = self.state.lock().await;
                    sm.mode.name() == "listening"
                };
                if is_listening {
                    if let Some(ref trans) = translation {
                        self.callback.show_translation(trans);
                    }
                }
            }
            asr::AsrEvent::SentenceEnd {
                utterance_id,
                text,
                translation,
                speaker,
                reason,
            } => {
                // Measure translation latency (Final → SentenceEnd gap)
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let final_time = LAST_FINAL_TIME.load(std::sync::atomic::Ordering::Relaxed);
                let mut translation_wait_ms: Option<u64> = None;
                if final_time > 0 {
                    let trans_lat = now_ms.saturating_sub(final_time);
                    if trans_lat < 10000 {
                        TRANS_LATENCY_MS.store(trans_lat, std::sync::atomic::Ordering::Relaxed);
                        translation_wait_ms = Some(trans_lat);
                    }
                    LAST_FINAL_TIME.store(0, std::sync::atomic::Ordering::Relaxed);
                }

                let has_trans = translation.as_ref().is_some_and(|s| !s.trim().is_empty());
                self.event_log.log(
                    "sentence_end",
                    serde_json::json!({
                        "utteranceId": utterance_id,
                        "text": &text,
                        "textChars": text.chars().count(),
                        "translation": &translation,
                        "translationChars": translation.as_ref().map(|s| s.chars().count()).unwrap_or(0),
                        "hasTranslation": has_trans,
                        "translationWaitMs": translation_wait_ms,
                        "speaker": speaker,
                        "reason": reason,
                        "emptyText": text.trim().is_empty()
                    }),
                );
                log::info!(
                    "SentenceEnd: \"{}\" trans={} reason={} ({}ms wait)",
                    &text.chars().take(40).collect::<String>(),
                    if has_trans { "yes" } else { "none" },
                    reason,
                    TRANS_LATENCY_MS.load(std::sync::atomic::Ordering::Relaxed)
                );

                // Don't call show_asr here — Final already displayed the text.
                // SentenceEnd only handles translation, TTS, commands, suggestions.
                let is_listening = {
                    let sm = self.state.lock().await;
                    sm.mode.name() == "listening"
                };
                if is_listening {
                    if let Some(ref trans) = translation {
                        self.callback.show_translation(trans);
                    } else {
                        log::info!(
                            "No Soniox translation for \"{}\" (ML Kit fallback in Kotlin)",
                            &text.chars().take(40).collect::<String>()
                        );
                    }
                }

                // Add to rolling conversation history
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    if let Ok(mut history) = self.conversation_history.write() {
                        history.push((speaker, trimmed.to_string()));
                        while history.len() > 10 {
                            history.remove(0);
                        }
                    }
                }

                // Mark sentence end time for latency measurement
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                LAST_SENTENCE_END.store(now, std::sync::atomic::Ordering::Relaxed);

                // Process sentence end (commands, TTS, suggestions, dialogue)
                self.on_sentence_end(&text, translation.as_deref(), speaker)
                    .await;
            }
            asr::AsrEvent::Error { message } => {
                log::error!("ASR event error: {}", message);
                self.event_log
                    .log("asr_error", serde_json::json!({"message": &message}));
                let lower = message.to_ascii_lowercase();
                let status = if lower.contains("connect_failed")
                    || lower.contains("timeout")
                    || lower.contains("lookup")
                    || lower.contains("resolve")
                    || lower.contains("network")
                    || lower.contains("host")
                {
                    "NET BAD"
                } else {
                    "ASR WAIT"
                };
                self.callback.show_status(status);
            }
        }
    }

    /// Process a completed sentence — TTS, echo suppression, commands, suggestions, dialogue
    async fn on_sentence_end(&self, text: &str, translation: Option<&str>, speaker: i32) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let mode_name = {
            let sm = self.state.lock().await;
            sm.mode.name().to_string()
        };

        log::info!(
            "Sentence end [{}]: speaker={} text=\"{}\"",
            mode_name,
            speaker,
            text
        );

        // TTS: read translation aloud in listening mode
        if mode_name == "listening" {
            if let Some(trans) = translation {
                if self.tts_enabled.load(std::sync::atomic::Ordering::Relaxed) && !trans.is_empty()
                {
                    {
                        let mut guard = self.tts_echo_guard.lock().await;
                        guard.record_spoken(trans);
                    }
                    let lang = if asr::is_japanese(trans) { "ja" } else { "zh" };
                    self.callback.speak(trans, lang);
                }
            }
        }

        // In interactive modes, any speech activity resets timeout
        if mode_name != "listening" {
            let mut sm = self.state.lock().await;
            if sm.check_timeout() {
                log::info!("Mode timed out before processing new sentence, returning to listening");
                self.callback.show_mode("listening");
                self.callback.show_status(self.msg.get("engine.started"));
                self.callback.show_ai_response("");
                return;
            }
            sm.last_activity = std::time::Instant::now();
        }

        // Echo suppression: skip if this is TTS playback picked up by mic
        {
            let guard = self.tts_echo_guard.lock().await;
            if guard.is_echo(text) {
                return;
            }
        }

        // Check voice commands (mode-aware)
        if let Some(command) = self.cmd.detect_in_mode(text, &mode_name) {
            if command == cmd::Command::EnterDialogue {
                self.handle_command(command).await;
                let question = text
                    .replace("クロード", "")
                    .replace("プロード", "")
                    .replace("くろーど", "")
                    .replace("claude", "")
                    .replace("Claude", "")
                    .replace("克劳德", "")
                    .trim_start_matches(['、', ',', ' ', '，'])
                    .trim()
                    .to_string();
                if !question.is_empty() && question.len() > 1 {
                    log::info!("Dialogue: first message: \"{}\"", question);
                    self.callback.show_asr(&question, -1, true);
                    // Don't add_user_message here — agent loop does it
                    let _ = self.agent_tx.send(AgentMessage::UserText(question)).await;
                }
                return;
            }
            self.handle_command(command).await;
            return;
        }

        // Route based on mode
        match mode_name.as_str() {
            "listening" => {
                if self.config.suggest_enabled && text.len() > 3 {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let last_suggest = self.last_suggest_ms.load(Ordering::Relaxed);
                    if now_ms.saturating_sub(last_suggest) < SUGGESTION_COOLDOWN_MS {
                        log::debug!(
                            "Suggest skipped: cooldown {}ms",
                            now_ms.saturating_sub(last_suggest)
                        );
                        return;
                    }
                    if self
                        .suggest_inflight
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                        .is_err()
                    {
                        log::debug!("Suggest skipped: request already in flight");
                        return;
                    }
                    self.last_suggest_ms.store(now_ms, Ordering::Relaxed);

                    let cb = self.callback.clone();
                    let ai = self.ai.clone();
                    let mut history = self.conversation_history();
                    // Don't duplicate — history already has speaker-labeled version
                    if history.is_empty() || !history.last().is_some_and(|h| h.contains(text)) {
                        history.push(text.to_string());
                    }
                    log::info!(
                        "Suggest: {} history items, latest=\"{}\"",
                        history.len(),
                        history.last().unwrap_or(&String::new())
                    );
                    let mem = self.memory.get();
                    let guide = self.guide.get();
                    let event_log = self.event_log.clone();
                    let suggest_inflight = self.suggest_inflight.clone();
                    tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        match ai.suggest(&history, &mem, guide.as_ref()).await {
                            Ok(pack) => {
                                let duration_ms = start.elapsed().as_millis() as u64;
                                let suggestion_count = pack.suggestions.len();
                                let mood = pack.mood.clone();
                                event_log.log(
                                    "suggestion_metrics",
                                    serde_json::json!({
                                        "success": true,
                                        "durationMs": duration_ms,
                                        "historyItems": history.len(),
                                        "suggestions": suggestion_count,
                                        "mood": mood
                                    }),
                                );
                                if let Some(mood) = pack.mood {
                                    cb.show_status(&format!("MOOD:{}", mood));
                                }
                                cb.show_suggestions(pack.suggestions);
                            }
                            Err(e) => {
                                let duration_ms = start.elapsed().as_millis() as u64;
                                let error = e.to_string();
                                event_log.log(
                                    "suggestion_metrics",
                                    serde_json::json!({
                                        "success": false,
                                        "durationMs": duration_ms,
                                        "historyItems": history.len(),
                                        "error": error
                                    }),
                                );
                                log::error!("Suggest error: {}", e);
                            }
                        }
                        suggest_inflight.store(false, Ordering::Release);
                    });
                }
            }
            "dialogue" => {
                // Don't add_user_message here — agent loop does it serially
                let _ = self
                    .agent_tx
                    .send(AgentMessage::UserText(text.to_string()))
                    .await;
            }
            _ => {}
        }
    }

    /// Create agent state for spawning the agent loop externally
    pub fn create_agent_state(&self) -> AgentState {
        AgentState {
            api_key: self.ai.api_key().to_string(),
            client: self.ai.client().clone(),
            callback: self.callback.clone(),
            echo_guard: self.tts_echo_guard.clone(),
            memory: self.memory.clone(),
            tools_json: self.tool_registry.to_api_json(),
            tts_enabled: self.tts_enabled.clone(),
            state: self.state.clone(),
            last_response: self.last_response.clone(),
            user_speaker: self.user_speaker.clone(),
            conversation_history: self.conversation_history.clone(),
            guide: self.guide.clone(),
        }
    }

    async fn handle_command(&self, command: cmd::Command) {
        log::info!("Handling command: {:?}", command);
        match command {
            cmd::Command::EnterDialogue => {
                let mut sm = self.state.lock().await;
                sm.enter_dialogue();
                self.callback.show_mode("dialogue");
                self.callback.show_status("💬 Claude");
                self.callback.show_translation("");
                log::info!("Entered dialogue mode");
            }
            cmd::Command::ExitDialogue => {
                let mut sm = self.state.lock().await;
                sm.exit_to_listening();
                self.callback.show_mode("listening");
                self.callback.show_status(self.msg.get("engine.started"));
                self.callback.show_ai_response("");
                self.callback.show_translation("");
                log::info!("Exited dialogue mode");
            }
            cmd::Command::EnterGeminiLive => {
                let mut sm = self.state.lock().await;
                sm.enter_gemini_live();
                self.callback.show_mode("gemini_live");
                self.callback.show_status("🎙 Gemini Live");
                self.callback.show_translation("");
                self.callback.show_ai_response("");
                log::info!("Entered Gemini Live mode");
            }
            cmd::Command::SessionReset => {
                self.memory.clear();
                self.guide.deactivate();
                self.show_guide_status();
                self.clear_history();
                {
                    let mut lr = self.last_response.lock().await;
                    lr.clear();
                }
                let mut sm = self.state.lock().await;
                sm.exit_to_listening();
                self.callback.show_mode("listening");
                self.callback.show_status(self.msg.get("session.reset"));
                self.callback.show_ai_response("");
                self.callback.show_translation("");
                self.callback.on_memory_updated("");
                log::info!("Session reset: memory, history, dialogue all cleared");
            }
            cmd::Command::ForceSuggest => {
                let cb = self.callback.clone();
                let ai = self.ai.clone();
                let history = self.conversation_history();
                let mem = self.memory.get();
                let guide = self.guide.get();
                tokio::spawn(async move {
                    match ai.suggest(&history, &mem, guide.as_ref()).await {
                        Ok(pack) => {
                            if let Some(mood) = pack.mood {
                                cb.show_status(&format!("MOOD:{}", mood));
                            }
                            cb.show_suggestions(pack.suggestions);
                        }
                        Err(e) => log::error!("Suggest error: {}", e),
                    }
                });
            }
            cmd::Command::ToggleTts => {
                let was = self
                    .tts_enabled
                    .fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
                let now = !was;
                self.callback
                    .show_status(if now { "🔊 TTS ON" } else { "🔇 TTS OFF" });
                log::info!("TTS toggled: {}", if now { "ON" } else { "OFF" });
            }
            cmd::Command::TakePhoto => {
                // Enter dialogue mode if not already, then request photo
                {
                    let mut sm = self.state.lock().await;
                    if !sm.mode.is_dialogue() {
                        sm.enter_dialogue();
                        self.callback.show_mode("dialogue");
                    }
                }
                self.callback.show_status("📷 ...");
                self.callback.take_photo();
                log::info!("Photo requested");
            }
            cmd::Command::ClearMemory => {
                self.memory.clear();
                self.callback.on_memory_updated("");
                self.callback.show_ai_response("Memory cleared");
                self.callback.speak("Memory cleared", "en");
                log::info!("Memory cleared by voice command");
            }
            cmd::Command::ShowStatus => {
                let mem = self.memory.get();
                let mem_count = if mem.is_empty() {
                    0
                } else {
                    mem.lines().count()
                };
                let tts = if self.tts_enabled.load(std::sync::atomic::Ordering::Relaxed) {
                    "ON"
                } else {
                    "OFF"
                };
                let mode = {
                    let sm = self.state.lock().await;
                    sm.mode.name().to_string()
                };
                let uptime = self.start_time.elapsed();
                let uptime_str = if uptime.as_secs() >= 3600 {
                    format!(
                        "{}h{}m",
                        uptime.as_secs() / 3600,
                        (uptime.as_secs() % 3600) / 60
                    )
                } else {
                    format!("{}m", uptime.as_secs() / 60)
                };
                let status = format!(
                    "{} | TTS:{} | Mem:{} | Up:{}",
                    mode, tts, mem_count, uptime_str
                );
                self.callback.show_asr(&status, -1, true);
                // Show memory preview in translation area
                if !mem.is_empty() {
                    let preview = mem.lines().take(3).collect::<Vec<_>>().join(" / ");
                    self.callback.show_translation(&preview);
                }
                self.callback.speak(&status, "en");
                log::info!("Status: {} | Memory: {}", status, mem.replace('\n', " | "));
            }
            cmd::Command::OpenSettings => {
                self.callback.show_mode("settings");
                // Return to listening after 30s auto-timeout
                let cb = self.callback.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    cb.show_mode("listening");
                });
                log::info!("Opened settings (auto-close 30s)");
            }
            cmd::Command::ToggleGuide => {
                if self.guide.is_active() {
                    self.guide.deactivate();
                    self.show_guide_status();
                    self.callback.show_asr("指導モード OFF", -1, true);
                } else {
                    self.callback
                        .show_asr("指導モード: Claude に設定を依頼してください", -1, true);
                    self.callback
                        .show_translation("说: \"克劳德，打开指导模式，目标是...\"");
                }
            }
            cmd::Command::ShowHelp => {
                let help = "【語音命令】\n\
                    克劳德/Claude → 对话模式\n\
                    结束对话/終了 → 退出对话\n\
                    拍照/写真 → 拍照\n\
                    静音/ミュート → TTS开关\n\
                    建议/提案 → 强制推荐\n\
                    状态/ステータス → 显示状态\n\
                    重复/もう一度 → 重复上次\n\
                    清除记忆 → 清空记忆\n\
                    使用说明/help → 本帮助";
                self.callback.show_asr("使用说明", -1, true);
                self.callback.show_translation(help);
                log::info!("ShowHelp displayed");
            }
            cmd::Command::RepeatLast => {
                let lr = self.last_response.lock().await;
                if !lr.is_empty() {
                    self.callback.show_translation(&lr);
                    let lang = if lr.chars().any(|c| ('\u{3040}'..='\u{30FF}').contains(&c)) {
                        "ja"
                    } else {
                        "zh"
                    };
                    self.callback.speak(&lr, lang);
                }
            }
        }
    }

    /// Quick LLM translation fallback when Soniox doesn't provide one
    #[allow(dead_code)]
    async fn llm_translate(
        client: &reqwest::Client,
        api_key: &str,
        text: &str,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "model": "anthropic/claude-haiku-4-5-20251001",
            "max_tokens": 100,
            "messages": [{
                "role": "user",
                "content": format!("Translate to Japanese. Output ONLY the translation, nothing else:\n{}", text)
            }]
        });

        let http_fut = async {
            let resp = client
                .post("https://openrouter.ai/api/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", api_key))
                .json(&body)
                .send()
                .await?;
            resp.text().await
        };

        match tokio::time::timeout(std::time::Duration::from_secs(10), http_fut).await {
            Ok(Ok(resp_text)) => {
                let json: serde_json::Value =
                    serde_json::from_str(&resp_text).map_err(|e| e.to_string())?;
                let content = json["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if content.is_empty() {
                    Err("empty".into())
                } else {
                    log::info!(
                        "LLM translate: \"{}\" → \"{}\"",
                        &text.chars().take(30).collect::<String>(),
                        &content.chars().take(30).collect::<String>()
                    );
                    Ok(content)
                }
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("timeout".into()),
        }
    }

    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        *running = false;
        self.asr.disconnect().await;
        log::info!("Engine stopped");
    }
}

// === Serial Agent Loop ===

impl AgentState {
    /// Build system prompt with memory, time, and ASR context
    fn build_system_prompt(&self) -> String {
        let mem = self.memory.get();
        let mem_note = if mem.is_empty() {
            String::new()
        } else {
            format!(
                "\nUser's saved notes: {}\n\
                 Treat saved notes as background only. Use them only when directly relevant to the current user request, ASR context, or active guide mode. Ignore stale or unrelated notes, and never introduce medical, appointment, medication, or personal-data topics unless the current context is already about them.",
                mem
            )
        };

        let user_spk = self.user_speaker.load(std::sync::atomic::Ordering::Relaxed);
        let asr_history = self
            .conversation_history
            .read()
            .map(|entries| {
                entries
                    .iter()
                    .map(|(spk, text)| {
                        let label = speaker_label(*spk, user_spk);
                        format!("{}: {}", label, text)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let asr_note = if asr_history.is_empty() {
            String::new()
        } else {
            format!("\nRecent ASR (overheard conversation):\n{}", asr_history)
        };

        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let h = (secs / 3600 + 9) % 24;
        let m = (secs / 60) % 60;
        format!(
            "You are ArOS, an AI assistant on AR smart glasses.\n\
             Current time: {:02}:{:02} JST\n\
             Rules:\n\
             - Reply in 1-2 sentences MAX. Extremely concise.\n\
             - Match the user's language.\n\
             - No markdown, no formatting — plain text only.\n\
             - No emoji.\n\
             - Use update_memory tool when user asks to remember/forget/note something.\n\
             - Use take_photo tool when user wants to see/identify something.\n\
             - When asked about your tools/capabilities, describe them in text. Do NOT call tools to demonstrate.\n\
             - Recent ASR uses P1/P2 speaker labels from Soniox. Treat P1 and P2 as separate people; do not merge their statements.\n\
             - Use set_user_speaker tool when the user identifies which speaker they are. Match their description to the ASR history below.\
             {}{}{}", h, m, mem_note, asr_note,
            if let Some(g) = self.guide.get() {
                format!("\n[Guide Mode] Goal: {} | Context: {}", g.goal, g.context)
            } else { String::new() }
        )
    }

    /// Call Claude API — returns (finish_reason, message JSON, full response text)
    async fn call_claude(
        &self,
        messages: &[serde_json::Value],
    ) -> Result<(String, serde_json::Value, String), String> {
        let body = serde_json::json!({
            "model": "anthropic/claude-sonnet-4.6",
            "max_tokens": 300,
            "messages": messages,
            "tools": self.tools_json
        });

        let start = std::time::Instant::now();
        log::info!("Agent: sending {} messages", messages.len());

        let http_fut = async {
            let resp = self
                .client
                .post("https://openrouter.ai/api/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            Ok::<(reqwest::StatusCode, String), reqwest::Error>((status, text))
        };

        match tokio::time::timeout(std::time::Duration::from_secs(30), http_fut).await {
            Ok(Ok((status, resp_text))) => {
                let ms = start.elapsed().as_millis();
                log::info!("Agent: response {}ms HTTP {}", ms, status.as_u16());

                if !status.is_success() {
                    return Err(format!(
                        "HTTP {}: {}",
                        status,
                        &resp_text.chars().take(200).collect::<String>()
                    ));
                }
                let json: serde_json::Value =
                    serde_json::from_str(&resp_text).map_err(|e| format!("JSON parse: {}", e))?;

                let finish = json["choices"][0]["finish_reason"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let message = json["choices"][0]["message"].clone();
                log::info!(
                    "Agent: finish={} content_type={} tool_calls_type={}",
                    finish,
                    if message["content"].is_string() {
                        "string"
                    } else if message["content"].is_null() {
                        "null"
                    } else {
                        "other"
                    },
                    if message.get("tool_calls").is_none() {
                        "absent"
                    } else if message["tool_calls"].is_null() {
                        "null"
                    } else if message["tool_calls"].is_array() {
                        "array"
                    } else {
                        "other"
                    }
                );
                Ok((finish, message, resp_text))
            }
            Ok(Err(e)) => Err(format!("HTTP error: {}", e)),
            Err(_) => Err("timeout (30s)".to_string()),
        }
    }

    /// Execute a single tool call, return result string
    fn execute_tool(&self, name: &str, args: &serde_json::Value) -> String {
        log::info!("Tool: {} args={}", name, args);
        self.callback.show_status(&format!("🔧 {}", name));

        match name {
            "update_memory" => {
                let action = args["action"].as_str().unwrap_or("add");
                let content = args["content"].as_str().unwrap_or("");
                let current = self.memory.get();
                match action {
                    "add" => {
                        let new = if current.is_empty() {
                            content.to_string()
                        } else {
                            format!("{}\n{}", current, content)
                        };
                        self.memory.set(&new);
                        self.callback.on_memory_updated(&new);
                        log::info!("Memory add: \"{}\"", content);
                        format!("Added to memory: {}", content)
                    }
                    "delete" => {
                        let new = current
                            .lines()
                            .filter(|l| !l.contains(content))
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.memory.set(&new);
                        self.callback.on_memory_updated(&new);
                        format!("Removed from memory: {}", content)
                    }
                    "replace_all" => {
                        self.memory.set(content);
                        self.callback.on_memory_updated(content);
                        "Memory replaced".to_string()
                    }
                    _ => "Unknown action".to_string(),
                }
            }
            "take_photo" => {
                self.callback.show_status("📷 capturing...");
                self.callback.take_photo();
                // Photo is async — result arrives via AgentMessage::Photo
                // Return special marker so handle_user_text knows to break the tool loop
                "PHOTO_PENDING".to_string()
            }
            "set_guide_mode" => {
                let active = args["active"].as_bool().unwrap_or(false);
                if active {
                    let label = args["label"].as_str().unwrap_or("📋 Guide");
                    let goal = args["goal"].as_str().unwrap_or("");
                    let context = args["context"].as_str().unwrap_or("");
                    self.guide.activate(label, goal, context);
                    self.callback.show_status(&format!("GUIDE:{}", label));
                    format!("Guide mode activated: {} — {}", label, goal)
                } else {
                    self.guide.deactivate();
                    self.callback.show_status("GUIDE:");
                    "Guide mode deactivated".to_string()
                }
            }
            "set_user_speaker" => {
                let id = args["speaker_id"].as_i64().unwrap_or(-1) as i32;
                self.user_speaker
                    .store(id, std::sync::atomic::Ordering::Relaxed);
                log::info!("Tool: set_user_speaker to {}", id);
                self.callback
                    .show_status(&format!("👤 You = Speaker {}", id));
                format!("User speaker set to {}. Conversation history will now label this speaker as [You].", id)
            }
            _ => format!("Unknown tool: {}", name),
        }
    }

    /// Display response: show on HUD, echo guard, TTS (no-op if no longer in dialogue)
    async fn display_response(&self, content: &str) {
        if content.is_empty() {
            return;
        }
        let is_dialogue = {
            let sm = self.state.lock().await;
            sm.mode.is_dialogue()
        };
        if !is_dialogue {
            log::info!("Agent: discarding stale response (no longer in dialogue)");
            return;
        }
        {
            let mut sm = self.state.lock().await;
            sm.add_assistant_message(content);
        }
        {
            let mut lr = self.last_response.lock().await;
            *lr = content.to_string();
        }
        self.callback.show_ai_response(content);
        self.callback.show_status("💬 Claude");
        {
            let mut guard = self.echo_guard.lock().await;
            guard.record_spoken(content);
        }
        let lang = if content
            .chars()
            .any(|c| ('\u{3040}'..='\u{30FF}').contains(&c))
        {
            "ja"
        } else {
            "zh"
        };
        if self.tts_enabled.load(std::sync::atomic::Ordering::Relaxed) {
            self.callback.speak(content, lang);
        }
    }

    /// Handle user text — serial agent loop with tool call iteration
    async fn handle_user_text(&self) {
        self.callback.show_status("💬 thinking...");

        let max_tool_iterations = 5;
        let mut handled = false;
        for iteration in 0..max_tool_iterations {
            // Build messages from current state each iteration (includes tool results)
            let system = self.build_system_prompt();
            let mut messages = vec![serde_json::json!({"role": "system", "content": system})];
            messages.extend(self.state.lock().await.api_messages());

            match self.call_claude(&messages).await {
                Ok((finish_reason, message, _raw)) => {
                    // Check for tool calls
                    // Only enter tool branch if there are ACTUAL tool calls (not just finish_reason)
                    let has_tools = message
                        .get("tool_calls")
                        .and_then(|v| v.as_array())
                        .is_some_and(|a| !a.is_empty());

                    if has_tools {
                        // Show any text content alongside tool calls
                        if let Some(text) = message["content"].as_str() {
                            if !text.is_empty() {
                                self.callback.show_ai_response(text);
                            }
                        }

                        // Store the raw assistant message (with tool_calls)
                        {
                            let mut sm = self.state.lock().await;
                            sm.add_raw_assistant_message(message.clone());
                        }

                        // Execute tools and store results
                        let mut photo_pending = false;
                        if let Some(tool_calls) = message["tool_calls"].as_array() {
                            log::info!(
                                "Agent: {} tool calls (iteration {})",
                                tool_calls.len(),
                                iteration
                            );
                            for tc in tool_calls {
                                let tc_id = tc["id"].as_str().unwrap_or("");
                                let name = tc["function"]["name"].as_str().unwrap_or("");
                                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                                let args: serde_json::Value =
                                    serde_json::from_str(args_str).unwrap_or_default();

                                let result = self.execute_tool(name, &args);

                                if result == "PHOTO_PENDING" {
                                    photo_pending = true;
                                    // Don't store placeholder — photo will arrive via channel
                                    let mut sm = self.state.lock().await;
                                    sm.add_tool_result(
                                        tc_id,
                                        "Photo capture in progress. Wait for the image.",
                                    );
                                } else {
                                    let mut sm = self.state.lock().await;
                                    sm.add_tool_result(tc_id, &result);
                                }
                            }
                        }

                        if photo_pending {
                            // Break tool loop — photo arrives as AgentMessage::Photo
                            log::info!("Agent: waiting for photo capture (breaking tool loop)");
                            break;
                        }

                        // Loop continues — next iteration sends tool results back to Claude
                        self.callback.show_status("💬 thinking...");
                        continue;
                    }

                    // Normal text response — display and exit loop
                    let content = message["content"].as_str().unwrap_or("(no response)");
                    let preview: String = content.chars().take(50).collect();
                    log::info!(
                        "Claude: \"{}\" (has_tools={}, finish={})",
                        preview,
                        has_tools,
                        finish_reason
                    );
                    self.display_response(content).await;
                    log::info!("Agent: display_response returned");
                    handled = true;
                    break;
                }
                Err(e) => {
                    log::error!("Agent error: {}", e);
                    self.callback.show_status(&format!("❌ {}", e));
                    break;
                }
            }
        }

        // Ensure waiting_for_response is cleared even on error/exhaustion
        if !handled {
            let mut sm = self.state.lock().await;
            if let state::Mode::Dialogue {
                waiting_for_response,
                ..
            } = &mut sm.mode
            {
                *waiting_for_response = false;
            }
        }
    }

    /// Handle photo — add image to chat history, then run the same tool-call loop
    async fn handle_photo(&self, base64_jpeg: &str) {
        let photo_start = std::time::Instant::now();
        self.callback.show_status("📷 analyzing...");

        {
            let mut sm = self.state.lock().await;
            sm.add_user_image(base64_jpeg, "What do you see in this photo?");
        }

        // Reuse the same serial agent loop as handle_user_text
        self.handle_user_text().await;
        log::info!(
            "Photo: total pipeline {}ms",
            photo_start.elapsed().as_millis()
        );
    }
}

/// Run the serial agent loop — consumes messages one at a time
pub async fn run_agent_loop(mut rx: tokio::sync::mpsc::Receiver<AgentMessage>, agent: AgentState) {
    log::info!("Agent loop started");
    while let Some(msg) = rx.recv().await {
        // Check mode
        let is_dialogue = {
            let sm = agent.state.lock().await;
            sm.mode.is_dialogue()
        };
        if !is_dialogue {
            log::debug!("Agent: ignoring message (not in dialogue mode)");
            continue;
        }

        match msg {
            AgentMessage::UserText(ref text) => {
                // Add to history HERE (serial, not before enqueue)
                {
                    let mut sm = agent.state.lock().await;
                    sm.add_user_message(text);
                }
                agent.handle_user_text().await;
            }
            AgentMessage::Photo(base64) => {
                agent.handle_photo(&base64).await;
            }
        }
    }
    log::info!("Agent loop ended");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_echo_guard_basic() {
        let mut guard = TtsEchoGuard::new();
        assert!(!guard.is_echo("hello"));

        guard.record_spoken("こんにちは。何かお手伝いできることはありますか？");
        assert!(guard.is_echo("こんにちは。"));
        assert!(guard.is_echo("何かお手伝いできることはありますか。"));
        assert!(!guard.is_echo("明日の天気は？"));
    }

    #[test]
    fn test_echo_guard_normalize() {
        assert_eq!(TtsEchoGuard::normalize("hello。world！"), "helloworld");
        assert_eq!(TtsEchoGuard::normalize("テスト、です。"), "テストです");
        assert_eq!(TtsEchoGuard::normalize("  "), "");
    }

    #[test]
    fn test_echo_guard_empty() {
        let guard = TtsEchoGuard::new();
        assert!(!guard.is_echo(""));
        assert!(!guard.is_echo("。"));
    }

    #[test]
    fn test_echo_guard_fuzzy() {
        let mut guard = TtsEchoGuard::new();
        guard.record_spoken("東京タワーの高さは333メートルです");
        // Partial match — fuzzy 60% overlap should catch this
        assert!(guard.is_echo("東京タワーの高さは"));
    }
}
