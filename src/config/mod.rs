//! Configuration management — settings, API keys, preferences

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub soniox_key: String,
    pub openrouter_key: String,
    pub suggest_enabled: bool,
    pub tts_enabled: bool,
    pub layout_mode: LayoutMode,
    pub mic_mode: MicMode,
    pub color_scheme: ColorScheme,
    pub locale: Locale,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LayoutMode {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MicMode {
    VoiceCommunication, // AudioSource 7 — echo cancellation
    VoiceRecognition,   // AudioSource 6 — light processing
    Unprocessed,        // AudioSource 9 — raw
    Default,            // AudioSource 1
}

impl MicMode {
    /// Android AudioSource constant
    pub fn android_source(&self) -> i32 {
        match self {
            MicMode::VoiceCommunication => 7,
            MicMode::VoiceRecognition => 6,
            MicMode::Unprocessed => 9,
            MicMode::Default => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorScheme {
    pub text: Color,
    pub translation: Color,
    pub background: Color,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Convert to Android Color.argb() int
    pub fn to_android_int(&self) -> i32 {
        ((self.a as i32) << 24) | ((self.r as i32) << 16) | ((self.g as i32) << 8) | (self.b as i32)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Locale {
    pub asr_language: String,   // "ja", "zh", "en", "ko"
    pub translate_from: String, // "ja"
    pub translate_to: String,   // "zh"
    pub ui_language: String,    // "zh", "ja", "en", "ko"
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            asr_language: "ja".to_string(),
            translate_from: "ja".to_string(),
            translate_to: "zh".to_string(),
            ui_language: "zh".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            soniox_key: String::new(),
            openrouter_key: String::new(),
            suggest_enabled: true,
            tts_enabled: false,
            layout_mode: LayoutMode::Horizontal,
            mic_mode: MicMode::Unprocessed,
            color_scheme: ColorScheme {
                text: Color::rgba(0, 255, 0, 255),          // green
                translation: Color::rgba(255, 200, 0, 220), // yellow
                background: Color::rgba(0, 0, 0, 0), // transparent (black = see-through on AR)
            },
            locale: Locale::default(),
        }
    }
}

impl Config {
    /// Load from JSON file, or return default
    pub fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save to JSON file
    pub fn save(&self, path: &str) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert!(cfg.suggest_enabled);
        assert!(!cfg.tts_enabled);
        assert_eq!(cfg.layout_mode, LayoutMode::Horizontal);
        assert_eq!(cfg.mic_mode, MicMode::Unprocessed);
        assert_eq!(cfg.color_scheme.background.a, 0); // transparent
    }

    #[test]
    fn test_mic_mode_android_source() {
        assert_eq!(MicMode::VoiceCommunication.android_source(), 7);
        assert_eq!(MicMode::VoiceRecognition.android_source(), 6);
        assert_eq!(MicMode::Unprocessed.android_source(), 9);
        assert_eq!(MicMode::Default.android_source(), 1);
    }

    #[test]
    fn test_color_to_android() {
        let green = Color::rgba(0, 255, 0, 255);
        // 0xFF00FF00 as i32
        assert_eq!(green.to_android_int(), -16711936);
    }

    #[test]
    fn test_config_serialize_roundtrip() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.suggest_enabled, cfg.suggest_enabled);
        assert_eq!(restored.locale.asr_language, "ja");
    }

    #[test]
    fn test_config_load_missing_file() {
        let cfg = Config::load("/tmp/nonexistent_aros_config.json");
        assert!(cfg.suggest_enabled); // should return default
    }

    #[test]
    fn test_config_save_load_roundtrip() {
        let path = "/tmp/aros_test_config.json";
        let mut cfg = Config::default();
        cfg.suggest_enabled = false;
        cfg.soniox_key = "test_key".to_string();
        cfg.save(path).unwrap();

        let loaded = Config::load(path);
        assert!(!loaded.suggest_enabled);
        assert_eq!(loaded.soniox_key, "test_key");

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_locale_default() {
        let locale = Locale::default();
        assert_eq!(locale.asr_language, "ja");
        assert_eq!(locale.translate_from, "ja");
        assert_eq!(locale.translate_to, "zh");
        assert_eq!(locale.ui_language, "zh");
    }
}
