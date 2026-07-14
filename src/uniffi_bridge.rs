//! UniFFI bridge layer — exposes aros-core to Kotlin/Swift via UniFFI bindings
//!
//! This is a thin wrapper that translates between UniFFI-compatible types
//! and the internal Engine types. Replaces the hand-written JNI in android.rs.

use crate::{ai, config, DisplayCallback, Engine};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// UniFFI callback interface — Kotlin implements this
#[uniffi::export(callback_interface)]
pub trait ArOsCallback: Send + Sync {
    fn show_asr(&self, text: String, speaker: i32, is_final: bool);
    fn show_translation(&self, text: String);
    fn show_suggestions(&self, json: String); // JSON array of suggestions
    fn show_status(&self, status: String);
    fn show_mode(&self, mode: String);
    fn show_ai_response(&self, text: String);
    fn speak(&self, text: String, lang: String);
    fn take_photo(&self);
    fn on_memory_updated(&self, memory: String);
    fn on_error(&self, msg: String);
}

/// Bridge: adapts UniFFI ArOsCallback to internal DisplayCallback trait
struct CallbackBridge {
    callback: Box<dyn ArOsCallback>,
}

impl DisplayCallback for CallbackBridge {
    fn show_asr(&self, text: &str, speaker: i32, is_final: bool) {
        self.callback.show_asr(text.to_string(), speaker, is_final);
    }
    fn show_translation(&self, text: &str) {
        self.callback.show_translation(text.to_string());
    }
    fn show_suggestions(&self, items: Vec<ai::Suggestion>) {
        let json = serde_json::to_string(&items).unwrap_or_default();
        self.callback.show_suggestions(json);
    }
    fn show_status(&self, status: &str) {
        self.callback.show_status(status.to_string());
    }
    fn show_mode(&self, mode: &str) {
        self.callback.show_mode(mode.to_string());
    }
    fn show_ai_response(&self, text: &str) {
        self.callback.show_ai_response(text.to_string());
    }
    fn speak(&self, text: &str, lang: &str) {
        self.callback.speak(text.to_string(), lang.to_string());
    }
    fn take_photo(&self) {
        self.callback.take_photo();
    }
    fn on_memory_updated(&self, memory: &str) {
        self.callback.on_memory_updated(memory.to_string());
    }
    fn on_error(&self, msg: &str) {
        self.callback.on_error(msg.to_string());
    }
}

// Global state (replaces OnceLock<Mutex<Option<AndroidState>>> in android.rs)
struct EngineState {
    engine: Arc<Engine>,
    runtime: tokio::runtime::Runtime,
    audio_tx: tokio::sync::mpsc::Sender<Vec<i16>>,
}

static STATE: OnceLock<Mutex<Option<EngineState>>> = OnceLock::new();
static DROPPED_AUDIO_CHUNKS: AtomicU64 = AtomicU64::new(0);

fn get_state() -> &'static Mutex<Option<EngineState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

// === UniFFI exported functions ===

#[uniffi::export]
pub fn aros_init(callback: Box<dyn ArOsCallback>, soniox_key: String, openrouter_key: String) {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("ArOS"),
    );

    let bridge = CallbackBridge { callback };
    let cb: Arc<dyn DisplayCallback> = Arc::new(bridge);

    let mut cfg = config::Config::default();
    cfg.soniox_key = soniox_key;
    cfg.openrouter_key = openrouter_key;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4) // was 2: caused starvation when Vision + Camera blocked both threads
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (engine_inner, mut event_rx, agent_rx) = Engine::new(cb.clone(), cfg);
    let agent_state = engine_inner.create_agent_state();
    let engine = Arc::new(engine_inner);

    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(8);

    rt.block_on(async { engine.start().await });

    // Audio feed loop: Kotlin audio thread → async engine
    let engine_clone = engine.clone();
    rt.spawn(async move {
        while let Some(pcm) = audio_rx.recv().await {
            engine_clone.feed_audio(&pcm).await;
        }
    });

    // ASR event processing loop: receive loop → Engine handlers
    let engine_events = engine.clone();
    rt.spawn(async move {
        while let Some(event) = event_rx.recv().await {
            engine_events.handle_asr_event(event).await;
        }
        log::info!("ASR event processing loop ended");
    });

    // Serial agent loop: dialogue messages processed one at a time
    rt.spawn(async move {
        crate::run_agent_loop(agent_rx, agent_state).await;
    });

    *get_state().lock().unwrap_or_else(|e| e.into_inner()) = Some(EngineState {
        engine,
        runtime: rt,
        audio_tx,
    });

    log::info!("ArOS engine initialized (UniFFI)");
}

#[uniffi::export]
pub fn aros_feed_audio(pcm: Vec<i16>) {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        if let Err(e) = state.audio_tx.try_send(pcm) {
            if matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_)) {
                let dropped = DROPPED_AUDIO_CHUNKS.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped <= 3 || dropped % 100 == 0 {
                    log::warn!("Audio queue full; dropped {} realtime chunks", dropped);
                }
            }
        }
    }
}

#[uniffi::export]
pub fn aros_on_notification(app_name: String, title: String, body: String) {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        state.engine.on_notification(&app_name, &title, &body);
    }
}

#[uniffi::export]
pub fn aros_on_photo_taken(base64_jpeg: String) {
    // Take references then drop lock before block_on (avoid deadlock)
    let (engine, rt_handle) = {
        let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(state) => (state.engine.clone(), state.runtime.handle().clone()),
            None => return,
        }
    };
    rt_handle.block_on(async {
        engine.on_photo_taken(&base64_jpeg).await;
    });
}

#[uniffi::export]
pub fn aros_set_memory(memory: String) {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        state.engine.memory.set(&memory);
        log::info!("Memory restored: {} chars", memory.len());
    }
}

#[uniffi::export]
pub fn aros_set_guide(label: String, goal: String, context: String) {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        state.engine.guide.activate(&label, &goal, &context);
        // Notify UI via status (GUIDE: prefix parsed by StatusBarOverlay)
        state.engine.show_guide_status();
    }
}

#[uniffi::export]
pub fn aros_clear_guide() {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        state.engine.guide.deactivate();
        state.engine.show_guide_status();
    }
}

#[uniffi::export]
pub fn aros_exit_gemini_live() {
    let guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        let engine = state.engine.clone();
        state.runtime.spawn(async move {
            let mut sm = engine.state.lock().await;
            sm.exit_to_listening();
            engine.callback.show_mode("listening");
            engine.callback.show_status("🎤 Listening");
            engine.callback.show_ai_response("");
            log::info!("Exited Gemini Live mode via UniFFI");
        });
    }
}

#[uniffi::export]
pub fn aros_stop() {
    let mut guard = get_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.take() {
        state.runtime.block_on(async { state.engine.stop().await });
    }
    log::info!("ArOS engine stopped (UniFFI)");
}

// scaffolding included from lib.rs
