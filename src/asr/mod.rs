//! ASR engine — Soniox WebSocket streaming speech-to-text
//!
//! Event-driven architecture: receive loop emits AsrEvent to mpsc channel,
//! Engine consumes events asynchronously. No shared mutable state for ASR results.

mod parser;

pub use parser::*;

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const RECONNECT_DELAY_MS: u64 = 1000;
const STALE_TIMEOUT_MS: u64 = 30_000;
/// If audio is being sent but no Partial received for this long, Soniox is lagging — reconnect
const LAG_RESET_MS: u64 = 5_000;
const MAX_RECONNECT_DELAY_MS: u64 = 30_000;
const CONNECT_TIMEOUT_MS: u64 = 10_000;
const PING_INTERVAL_MS: u64 = 10_000;
const FINALIZE_COOLDOWN_MS: u64 = 1_500;
/// ASR event — emitted by receive loop, consumed by Engine
#[derive(Debug, Clone)]
pub enum AsrEvent {
    /// Partial recognition (text still changing)
    Partial {
        utterance_id: u64,
        text: String,
        speaker: i32,
    },
    /// Final recognition for current segment
    Final {
        utterance_id: u64,
        text: String,
        translation: Option<String>,
        speaker: i32,
        reason: &'static str,
    },
    /// Sentence complete — translation collected (or timed out)
    SentenceEnd {
        utterance_id: u64,
        text: String,
        translation: Option<String>,
        speaker: i32,
        reason: &'static str,
    },
    /// Connection/protocol error
    Error { message: String },
}

/// Translation grace period — wait this long after Final for translation to arrive
/// Grace period after sentence end — wait for Soniox finalize + translation
const TRANSLATION_GRACE_MS: u64 = 1200;

#[derive(Clone)]
pub struct AsrEngine {
    soniox_key: String,
    asr_language: String,
    translate_from: String,
    translate_to: String,
    state: Arc<Mutex<AsrState>>,
    /// Event channel — receive loop emits, Engine consumes
    event_tx: Arc<tokio::sync::mpsc::Sender<AsrEvent>>,
}

/// Connection state only — ASR result state is owned by the receive loop
struct AsrState {
    audio_buffer: std::collections::VecDeque<Vec<u8>>,
    ws_tx: Option<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
    connected: bool,
    connecting: bool,
    last_recv_time: std::time::Instant,
    last_ping_sent: std::time::Instant,
    last_connect_attempt: std::time::Instant,
    reconnect_delay_ms: u64,
    connect_failures: u32,
    /// Generation counter — incremented on each connect, used to cancel stale receive loops
    generation: u64,
    /// Set by feed() when lag detected, consumed by receive loop after SentenceEnd
    needs_reconnect: bool,
    /// Set by receive loop when silence splits a sentence; consumed by feed().
    needs_finalize: bool,
    last_finalize_sent: std::time::Instant,
}

impl AsrState {
    fn new() -> Self {
        Self {
            audio_buffer: std::collections::VecDeque::new(),
            ws_tx: None,
            connected: false,
            connecting: false,
            last_recv_time: std::time::Instant::now(),
            last_ping_sent: std::time::Instant::now(),
            last_connect_attempt: std::time::Instant::now() - std::time::Duration::from_secs(60),
            reconnect_delay_ms: RECONNECT_DELAY_MS,
            connect_failures: 0,
            generation: 0,
            needs_reconnect: false,
            needs_finalize: false,
            last_finalize_sent: std::time::Instant::now() - std::time::Duration::from_secs(60),
        }
    }
}

impl AsrEngine {
    pub fn new(soniox_key: &str) -> (Self, tokio::sync::mpsc::Receiver<AsrEvent>) {
        Self::with_languages(soniox_key, "ja", "ja", "zh")
    }

    pub fn with_languages(
        soniox_key: &str,
        asr_lang: &str,
        from: &str,
        to: &str,
    ) -> (Self, tokio::sync::mpsc::Receiver<AsrEvent>) {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
        let engine = Self {
            soniox_key: soniox_key.to_string(),
            asr_language: asr_lang.to_string(),
            translate_from: from.to_string(),
            translate_to: to.to_string(),
            state: Arc::new(Mutex::new(AsrState::new())),
            event_tx: Arc::new(event_tx),
        };
        (engine, event_rx)
    }

    /// Build language hints array — ja/zh/en trilingual
    fn language_hints(&self) -> Vec<String> {
        let mut hints = vec![self.asr_language.clone()];
        for lang in &["zh", "en", "ja"] {
            if *lang != self.asr_language && hints.len() < 3 {
                hints.push(lang.to_string());
            }
        }
        hints
    }

    /// Connect to Soniox WebSocket
    pub async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Prevent concurrent connect attempts
        {
            let mut state = self.state.lock().await;
            if state.connected || state.connecting {
                return Ok(());
            }
            state.connecting = true;
        }

        let url = "wss://stt-rt.soniox.com/transcribe-websocket";
        log::info!("ASR: connecting to {}", url);

        log::info!(
            "ASR: attempting WebSocket handshake ({}ms timeout)...",
            CONNECT_TIMEOUT_MS
        );
        let connect_result = tokio::time::timeout(
            std::time::Duration::from_millis(CONNECT_TIMEOUT_MS),
            connect_async(url),
        )
        .await;
        let (ws_stream, resp) = match connect_result {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                self.clear_connecting().await;
                return Err(Box::new(e));
            }
            Err(_) => {
                self.clear_connecting().await;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "WebSocket connect timeout",
                )
                .into());
            }
        };
        log::info!("ASR: WebSocket handshake done, status={:?}", resp.status());
        let (mut tx, mut rx) = ws_stream.split();

        // Send config
        let config = serde_json::json!({
            "api_key": self.soniox_key,
            "model": "stt-rt-preview",
            "audio_format": "pcm_s16le",
            "sample_rate": 16000,
            "num_channels": 1,
            "language_hints": self.language_hints(),
            "translation": {
                "type": "two_way",
                "language_a": &self.translate_from,
                "language_b": &self.translate_to
            },
            "speaker_diarization": {
                "num_speakers": 0
            },
            // Note: enable_endpoint_detection is NOT supported by stt-rt-preview
            // Translation is triggered by sending {"type":"finalize"} from feed()
        });
        if let Err(e) = tx.send(Message::Text(config.to_string())).await {
            self.clear_connecting().await;
            return Err(Box::new(e));
        }
        log::info!("ASR: connected, config sent");

        let my_generation;
        {
            let mut state = self.state.lock().await;
            state.generation += 1;
            my_generation = state.generation;
            state.ws_tx = Some(tx);
            state.connected = true;
            state.connecting = false;
            state.last_recv_time = std::time::Instant::now();
        }

        // Spawn event-driven receive loop
        let state = self.state.clone();
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            log::info!(
                "ASR: receive loop started (event-driven, gen={})",
                my_generation
            );
            let mut _msg_count = 0u64;
            let mut last_ping = std::time::Instant::now();

            // Per-utterance state (owned by receive loop, no shared mutex)
            let mut utterance_id: u64 = 0;
            let mut finalized = String::new();
            let mut translation = String::new();
            let mut current_speaker: i32 = -1;
            let mut last_partial = String::new();
            let mut awaiting_translation = false; // true after Final, waiting for translation grace
            let mut grace_deadline: Option<tokio::time::Instant> = None;
            let mut last_token_time: Option<tokio::time::Instant> = None; // for silence timeout
            const SILENCE_TIMEOUT_MS: u64 = 1600; // auto-finalize partial after short silence

            loop {
                // Use short timeout when awaiting translation or watching for silence
                let timeout_ms = if awaiting_translation {
                    TRANSLATION_GRACE_MS
                } else if last_token_time.is_some() && !last_partial.is_empty() {
                    SILENCE_TIMEOUT_MS
                } else {
                    PING_INTERVAL_MS
                };
                let msg =
                    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.next())
                        .await;

                // Check translation grace timeout
                if awaiting_translation {
                    if let Some(deadline) = grace_deadline {
                        if tokio::time::Instant::now() >= deadline {
                            // Grace period expired — emit SentenceEnd with whatever translation we have
                            let trans = if translation.is_empty() {
                                None
                            } else {
                                Some(translation.clone())
                            };
                            let _ = event_tx
                                .send(AsrEvent::SentenceEnd {
                                    utterance_id,
                                    text: finalized.clone(),
                                    translation: trans,
                                    speaker: current_speaker,
                                    reason: "translation_grace_timeout",
                                })
                                .await;
                            log::info!(
                                "ASR: SentenceEnd (grace timeout {}ms) id={} text=\"{}\" trans={}",
                                TRANSLATION_GRACE_MS,
                                utterance_id,
                                &finalized.chars().take(40).collect::<String>(),
                                if translation.is_empty() {
                                    "none".to_string()
                                } else {
                                    format!(
                                        "\"{}\"",
                                        &translation.chars().take(40).collect::<String>()
                                    )
                                }
                            );

                            // Reset for next utterance
                            utterance_id += 1;
                            finalized.clear();
                            translation.clear();
                            last_partial.clear();
                            awaiting_translation = false;
                            grace_deadline = None;
                        }
                    }
                }

                // After sentence end: check if lag reconnect was requested
                {
                    let s = state.lock().await;
                    if s.needs_reconnect {
                        log::warn!("ASR: reconnecting to drop lag queue");
                        break; // exit receive loop → triggers reconnect on next feed()
                    }
                }

                // Check silence timeout: auto-finalize partial after 3s of no new tokens
                // Instead of emitting SentenceEnd(None) immediately, enter grace period
                // to wait for translation — same path as punctuated sentences.
                if !awaiting_translation && !last_partial.is_empty() {
                    if let Some(token_time) = last_token_time {
                        if token_time.elapsed().as_millis() > SILENCE_TIMEOUT_MS as u128 {
                            // Silence timeout — emit Final and enter grace period for translation
                            finalized = last_partial.clone();
                            log::info!(
                                "ASR: silence timeout, entering grace for: \"{}\"",
                                &finalized.chars().take(30).collect::<String>()
                            );
                            {
                                let mut s = state.lock().await;
                                s.needs_finalize = true;
                            }
                            let trans = if translation.is_empty() {
                                None
                            } else {
                                Some(translation.clone())
                            };
                            let _ = event_tx
                                .send(AsrEvent::Final {
                                    utterance_id,
                                    text: finalized.clone(),
                                    translation: trans,
                                    speaker: current_speaker,
                                    reason: "silence_timeout",
                                })
                                .await;
                            // Enter grace period to collect translation
                            awaiting_translation = true;
                            last_token_time = None;
                            grace_deadline = Some(
                                tokio::time::Instant::now()
                                    + std::time::Duration::from_millis(TRANSLATION_GRACE_MS),
                            );
                        }
                    }
                }

                let msg = match msg {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break,
                    Err(_) => {
                        if !awaiting_translation
                            && last_ping.elapsed().as_millis() > PING_INTERVAL_MS as u128 * 2
                        {
                            log::warn!("ASR: no messages for {}s", last_ping.elapsed().as_secs());
                        }
                        continue;
                    }
                };
                last_ping = std::time::Instant::now();

                // Check if a newer connection superseded us
                {
                    let s = state.lock().await;
                    if s.generation != my_generation {
                        log::info!(
                            "ASR: receive loop gen={} superseded by gen={}, exiting",
                            my_generation,
                            s.generation
                        );
                        break;
                    }
                }

                match msg {
                    Ok(Message::Text(text)) => {
                        _msg_count += 1;
                        {
                            let mut s = state.lock().await;
                            s.last_recv_time = std::time::Instant::now();
                        }

                        if let Ok(parsed) = parser::parse_soniox_response(&text) {
                            // Handle grace period first — must resolve before processing new tokens
                            if awaiting_translation {
                                let has_new_asr =
                                    !parsed.finalized.is_empty() || parsed.has_partial;

                                if !parsed.translation.is_empty() {
                                    // Translation arrived — emit SentenceEnd with it
                                    translation.push_str(&parsed.translation);
                                    let _ = event_tx
                                        .send(AsrEvent::SentenceEnd {
                                            utterance_id,
                                            text: finalized.clone(),
                                            translation: Some(translation.clone()),
                                            speaker: current_speaker,
                                            reason: "translation_arrived",
                                        })
                                        .await;
                                    log::info!(
                                        "ASR: SentenceEnd (trans arrived) id={}",
                                        utterance_id
                                    );
                                    utterance_id += 1;
                                    finalized.clear();
                                    translation.clear();
                                    last_partial.clear();
                                    awaiting_translation = false;
                                    grace_deadline = None;
                                    // Fall through to process any new ASR tokens in same message
                                } else if has_new_asr {
                                    if grace_deadline
                                        .is_some_and(|deadline| tokio::time::Instant::now() < deadline)
                                    {
                                        // Soniox often sends the next ASR tokens a few milliseconds
                                        // before the translation token for the previous sentence.
                                        // Do not cut the previous sentence short; cumulative partials
                                        // will refresh the next utterance after the grace window.
                                        continue;
                                    }
                                    // New ASR text arrived before translation — emit SentenceEnd without it
                                    let trans = if translation.is_empty() {
                                        None
                                    } else {
                                        Some(translation.clone())
                                    };
                                    let _ = event_tx
                                        .send(AsrEvent::SentenceEnd {
                                            utterance_id,
                                            text: finalized.clone(),
                                            translation: trans,
                                            speaker: current_speaker,
                                            reason: "new_asr_preempted",
                                        })
                                        .await;
                                    log::info!(
                                        "ASR: SentenceEnd (new ASR preempted) id={}",
                                        utterance_id
                                    );
                                    utterance_id += 1;
                                    finalized.clear();
                                    translation.clear();
                                    last_partial.clear();
                                    awaiting_translation = false;
                                    grace_deadline = None;
                                    // Fall through to process the new ASR tokens
                                } else {
                                    // No translation, no new ASR — keep waiting
                                    continue;
                                }
                            }

                            // Update speaker (after grace resolution to avoid leaking to wrong sentence)
                            if parsed.speaker >= 0 {
                                current_speaker = parsed.speaker;
                            }

                            // Accumulate translation (discard stale tokens from previous utterance)
                            if !parsed.translation.is_empty() {
                                if finalized.is_empty() {
                                    log::debug!(
                                        "ASR: discarding stale translation: \"{}\"",
                                        &parsed.translation.chars().take(30).collect::<String>()
                                    );
                                } else {
                                    translation.push_str(&parsed.translation);
                                }
                            }

                            // Accumulate finalized text
                            if !parsed.finalized.is_empty() {
                                finalized.push_str(&parsed.finalized);
                            }

                            // Emit Partial or Final
                            let full_text = format!("{}{}", finalized, parsed.partial);
                            if !full_text.is_empty() {
                                let is_final = parsed.partial.is_empty() && !finalized.is_empty();
                                // Auto-split: treat as sentence end if finalized is too long
                                // (English often lacks punctuation from Soniox)
                                let is_too_long = finalized.chars().count() > 60;
                                    let is_sentence_end =
                                        is_final && (is_sentence_ending(&finalized) || is_too_long);

                                if is_sentence_end {
                                    // Emit Final immediately
                                    let trans = if translation.is_empty() {
                                        None
                                    } else {
                                        Some(translation.clone())
                                    };
                                    let _ = event_tx
                                        .send(AsrEvent::Final {
                                            utterance_id,
                                            text: finalized.clone(),
                                            translation: trans,
                                            speaker: current_speaker,
                                            reason: if is_too_long {
                                                "too_long"
                                            } else {
                                                "sentence_punctuation"
                                            },
                                        })
                                        .await;

                                    // No finalize — ML Kit handles translation locally.
                                    // Finalize was causing massive lag by interrupting Soniox.

                                    // Start grace period to wait for translation tokens.
                                    awaiting_translation = true;
                                    last_token_time = None;
                                    grace_deadline = Some(
                                        tokio::time::Instant::now()
                                            + std::time::Duration::from_millis(
                                                TRANSLATION_GRACE_MS,
                                            ),
                                    );
                                } else if full_text != last_partial {
                                    // Deduplicated partial
                                    last_partial = full_text.clone();
                                    last_token_time = Some(tokio::time::Instant::now());
                                    let _ = event_tx
                                        .send(AsrEvent::Partial {
                                            utterance_id,
                                            text: full_text,
                                            speaker: current_speaker,
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        log::info!("ASR: WebSocket closed");
                        let _ = event_tx
                            .send(AsrEvent::Error {
                                message: "WebSocket closed".to_string(),
                            })
                            .await;
                        break;
                    }
                    Ok(Message::Pong(_)) => {
                        let mut s = state.lock().await;
                        s.last_recv_time = std::time::Instant::now();
                    }
                    Err(e) => {
                        log::error!("ASR: WebSocket error: {}", e);
                        let _ = event_tx
                            .send(AsrEvent::Error {
                                message: e.to_string(),
                            })
                            .await;
                        break;
                    }
                    _ => {}
                }
            }
            // Flush any pending SentenceEnd before exiting
            if awaiting_translation && !finalized.is_empty() {
                let trans = if translation.is_empty() {
                    None
                } else {
                    Some(translation.clone())
                };
                let _ = event_tx
                    .send(AsrEvent::SentenceEnd {
                        utterance_id,
                        text: finalized.clone(),
                        translation: trans,
                        speaker: current_speaker,
                        reason: "disconnect_flush",
                    })
                    .await;
                log::info!(
                    "ASR: flushed pending SentenceEnd on disconnect id={}",
                    utterance_id
                );
            }

            // Only clear connection state if we're still the current generation
            // (a newer connect() may have already replaced us)
            let mut s = state.lock().await;
            if s.generation == my_generation {
                s.connected = false;
                s.ws_tx = None;
                s.needs_reconnect = false;
            } else {
                log::info!(
                    "ASR: stale loop gen={} skipping cleanup (current={})",
                    my_generation,
                    s.generation
                );
            }
        });

        Ok(())
    }

    async fn clear_connecting(&self) {
        let mut state = self.state.lock().await;
        state.connecting = false;
    }

    /// Send PCM audio to WebSocket (results arrive via event channel, not return value)
    pub async fn feed(&self, pcm: &[i16]) {
        // Step 1: Check connection, reconnect with backoff
        {
            let mut state = self.state.lock().await;
            if !state.connected {
                if state.connecting {
                    return;
                }
                let elapsed = state.last_connect_attempt.elapsed().as_millis() as u64;
                if elapsed < state.reconnect_delay_ms {
                    return;
                }
                state.last_connect_attempt = std::time::Instant::now();
                drop(state);

                match self.connect().await {
                    Ok(_) => {
                        let mut s = self.state.lock().await;
                        s.reconnect_delay_ms = RECONNECT_DELAY_MS;
                        s.connect_failures = 0;
                        return;
                    }
                    Err(e) => {
                        let mut s = self.state.lock().await;
                        s.connecting = false;
                        s.connect_failures += 1;
                        s.reconnect_delay_ms =
                            (s.reconnect_delay_ms * 2).min(MAX_RECONNECT_DELAY_MS);
                        log::error!(
                            "ASR: connect failed (#{}, next retry in {}s): {}",
                            s.connect_failures,
                            s.reconnect_delay_ms / 1000,
                            e
                        );
                        let _ = self.event_tx.try_send(AsrEvent::Error {
                            message: format!("connect_failed: {}", e),
                        });
                        return;
                    }
                }
            }
        }

        // Step 2: Take tx out of lock, send audio, put back
        static FEED_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = FEED_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count < 5 || count % 500 == 0 {
            log::info!("ASR: feed #{} pcm_len={}", count, pcm.len());
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut taken_tx = {
            let mut state = self.state.lock().await;

            // Check lag: if no ASR response for LAG_RESET_MS while sending audio,
            // Soniox is processing a backlog. Set flag to reconnect after sentence end.
            if state.connected
                && state.last_recv_time.elapsed().as_millis() > LAG_RESET_MS as u128
                && count > 50
            {
                if !state.needs_reconnect {
                    log::warn!(
                        "ASR: lag detected ({}ms no response), will reconnect after sentence end",
                        state.last_recv_time.elapsed().as_millis()
                    );
                    state.needs_reconnect = true;
                }
            }

            // Check stale
            if state.last_recv_time.elapsed().as_millis() > STALE_TIMEOUT_MS as u128 {
                log::warn!(
                    "ASR: stale connection ({}s), reconnecting",
                    state.last_recv_time.elapsed().as_secs()
                );
                state.connected = false;
                state.connecting = false;
                state.ws_tx = None;
                state.last_connect_attempt = std::time::Instant::now();
                state.reconnect_delay_ms = RECONNECT_DELAY_MS;
                return;
            }

            state.ws_tx.take() // Take tx out so lock is released before send
        };
        // Lock released — receive loop can process while we send

        if let Some(ref mut tx) = taken_tx {
            // Drop any old buffered audio from reconnect periods. Real-time ASR should resume
            // from the current microphone stream, not replay stale street noise.
            {
                let mut state = self.state.lock().await;
                let buf_len = state.audio_buffer.len();
                if buf_len > 0 {
                    state.audio_buffer.clear();
                    log::info!("ASR: dropped {} stale buffered audio chunks", buf_len);
                }
            }
            // Send WebSocket ping if interval elapsed
            {
                let state = self.state.lock().await;
                if state.last_ping_sent.elapsed().as_millis() > PING_INTERVAL_MS as u128 {
                    drop(state);
                    let _ = tx.send(Message::Ping(vec![])).await;
                    let mut s = self.state.lock().await;
                    s.last_ping_sent = std::time::Instant::now();
                }
            }

            // Send current chunk
            let should_finalize = {
                let mut state = self.state.lock().await;
                if state.needs_finalize
                    && state.last_finalize_sent.elapsed().as_millis()
                        > FINALIZE_COOLDOWN_MS as u128
                {
                    state.needs_finalize = false;
                    state.last_finalize_sent = std::time::Instant::now();
                    true
                } else {
                    false
                }
            };
            if should_finalize {
                let msg = serde_json::json!({"type": "finalize"}).to_string();
                if let Err(e) = tx.send(Message::Text(msg)).await {
                    log::error!("ASR: finalize send failed: {}", e);
                    let mut state = self.state.lock().await;
                    state.connected = false;
                    return;
                }
                log::info!("ASR: finalize sent after silence timeout");
            }

            if let Err(e) = tx.send(Message::Binary(bytes)).await {
                log::error!("ASR: send failed: {}", e);
                let mut state = self.state.lock().await;
                state.connected = false;
                return;
            }
            if count < 5 {
                log::info!("ASR: audio sent OK #{}", count);
            }

            // Finalize is sent only after receive-loop silence splitting. Sending it on
            // every feed was causing massive lag by interrupting Soniox stream.
        } else {
            if count < 10 || count % 500 == 0 {
                log::info!("ASR: dropped audio #{} while reconnecting", count);
            }
        }

        // Put tx back
        {
            let mut state = self.state.lock().await;
            if state.connected {
                state.ws_tx = taken_tx;
            }
        }
    }

    pub async fn disconnect(&self) {
        let mut state = self.state.lock().await;
        state.connected = false;
        state.ws_tx = None;
        log::info!("ASR: disconnected");
    }
}

/// Check if text ends with sentence-ending punctuation
pub fn is_sentence_ending(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    matches!(
        trimmed.chars().last(),
        Some('。' | '？' | '！' | '.' | '?' | '!')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentence_ending() {
        assert!(is_sentence_ending("今日は天気がいいですね。"));
        assert!(is_sentence_ending("本当ですか？"));
        assert!(is_sentence_ending("すごい！"));
        assert!(is_sentence_ending("Hello."));
        assert!(!is_sentence_ending("今日は天気が"));
        assert!(!is_sentence_ending(""));
        assert!(!is_sentence_ending("   "));
    }
}
