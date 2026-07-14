//! Internationalization — locale-aware status messages and voice commands
//!
//! Supported: en, zh, ja, ko

use std::collections::HashMap;

pub struct Messages {
    strings: HashMap<&'static str, &'static str>,
    pub locale: String,
}

impl Messages {
    pub fn new(locale: &str) -> Self {
        let strings = match locale {
            "ja" => ja_strings(),
            "en" => en_strings(),
            "ko" => ko_strings(),
            _ => zh_strings(), // default: Chinese
        };
        Self {
            strings,
            locale: locale.to_string(),
        }
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).copied().unwrap_or(key)
    }
}

fn zh_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    // Engine status
    m.insert("engine.starting", "[启动中]");
    m.insert("engine.started", "[已启动]");
    m.insert("asr.connecting", "[ASR 连接中...]");
    m.insert("asr.connected", "[准备完了]");
    m.insert("asr.connect_failed", "[ASR 连接失败]");
    m.insert("asr.reconnecting", "[重新连接...]");
    m.insert("asr.listening", "[聆听中]");
    m.insert("session.reset", "[已重置]");
    m.insert("memory.editing", "[修改记忆]");
    m.insert("memory.updated", "[记忆已更新]");
    m.insert("memory.cancelled", "[已取消]");
    m.insert("net.offline", "离线");
    // Mode labels
    m.insert("mode.listening", "🎤 聆听");
    m.insert("mode.dialogue", "💬 对话");
    m.insert("mode.memory", "📝 记忆");
    // Dialogue
    m.insert("dialogue.thinking", "💬 思考中...");
    m.insert("dialogue.error", "❌ 对话出错");
    m
}

fn ja_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("engine.starting", "[起動中]");
    m.insert("engine.started", "[起動完了]");
    m.insert("asr.connecting", "[ASR接続中...]");
    m.insert("asr.connected", "[準備完了]");
    m.insert("asr.connect_failed", "[ASR接続失敗]");
    m.insert("asr.reconnecting", "[再接続中...]");
    m.insert("asr.listening", "[聴取中]");
    m.insert("session.reset", "[リセット完了]");
    m.insert("memory.editing", "[メモリ編集]");
    m.insert("memory.updated", "[メモリ更新完了]");
    m.insert("memory.cancelled", "[キャンセル]");
    m.insert("net.offline", "オフライン");
    m.insert("mode.listening", "🎤 聴取");
    m.insert("mode.dialogue", "💬 対話");
    m.insert("mode.memory", "📝 メモリ");
    m.insert("dialogue.thinking", "💬 考え中...");
    m.insert("dialogue.error", "❌ エラー");
    m
}

fn en_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("engine.starting", "[Starting]");
    m.insert("engine.started", "[Ready]");
    m.insert("asr.connecting", "[Connecting ASR...]");
    m.insert("asr.connected", "[Connected]");
    m.insert("asr.connect_failed", "[ASR Connection Failed]");
    m.insert("asr.reconnecting", "[Reconnecting...]");
    m.insert("asr.listening", "[Listening]");
    m.insert("session.reset", "[Session Reset]");
    m.insert("memory.editing", "[Edit Memory]");
    m.insert("memory.updated", "[Memory Updated]");
    m.insert("memory.cancelled", "[Cancelled]");
    m.insert("net.offline", "offline");
    m.insert("mode.listening", "🎤 Listen");
    m.insert("mode.dialogue", "💬 Claude");
    m.insert("mode.memory", "📝 Memory");
    m.insert("dialogue.thinking", "💬 Thinking...");
    m.insert("dialogue.error", "❌ Error");
    m
}

fn ko_strings() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("engine.starting", "[시작 중]");
    m.insert("engine.started", "[준비 완료]");
    m.insert("asr.connecting", "[ASR 연결 중...]");
    m.insert("asr.connected", "[연결됨]");
    m.insert("asr.connect_failed", "[ASR 연결 실패]");
    m.insert("asr.reconnecting", "[재연결 중...]");
    m.insert("asr.listening", "[듣는 중]");
    m.insert("session.reset", "[초기화 완료]");
    m.insert("memory.editing", "[메모리 편집]");
    m.insert("memory.updated", "[메모리 업데이트됨]");
    m.insert("memory.cancelled", "[취소됨]");
    m.insert("net.offline", "오프라인");
    m.insert("mode.listening", "🎤 듣기");
    m.insert("mode.dialogue", "💬 대화");
    m.insert("mode.memory", "📝 메모리");
    m.insert("dialogue.thinking", "💬 생각 중...");
    m.insert("dialogue.error", "❌ 오류");
    m
}

/// Supported ASR language codes for Soniox
pub const SUPPORTED_ASR_LANGUAGES: &[(&str, &str)] = &[
    ("ja", "Japanese"),
    ("zh", "Chinese"),
    ("en", "English"),
    ("ko", "Korean"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zh_messages() {
        let m = Messages::new("zh");
        assert_eq!(m.get("asr.connecting"), "[ASR 连接中...]");
        assert_eq!(m.get("engine.started"), "[已启动]");
        assert_eq!(m.get("mode.dialogue"), "💬 对话");
    }

    #[test]
    fn test_ja_messages() {
        let m = Messages::new("ja");
        assert_eq!(m.get("asr.connecting"), "[ASR接続中...]");
        assert_eq!(m.get("mode.memory"), "📝 メモリ");
    }

    #[test]
    fn test_en_messages() {
        let m = Messages::new("en");
        assert_eq!(m.get("asr.connected"), "[Connected]");
    }

    #[test]
    fn test_ko_messages() {
        let m = Messages::new("ko");
        assert_eq!(m.get("engine.started"), "[준비 완료]");
        assert_eq!(m.get("mode.listening"), "🎤 듣기");
        assert_eq!(m.get("dialogue.thinking"), "💬 생각 중...");
    }

    #[test]
    fn test_unknown_locale_defaults_to_zh() {
        let m = Messages::new("fr");
        assert_eq!(m.get("engine.starting"), "[启动中]");
    }

    #[test]
    fn test_unknown_key_returns_key() {
        let m = Messages::new("en");
        assert_eq!(m.get("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn test_all_locales_have_same_keys() {
        let zh = zh_strings();
        let ja = ja_strings();
        let en = en_strings();
        let ko = ko_strings();
        for key in zh.keys() {
            assert!(ja.contains_key(key), "ja missing key: {}", key);
            assert!(en.contains_key(key), "en missing key: {}", key);
            assert!(ko.contains_key(key), "ko missing key: {}", key);
        }
    }
}
