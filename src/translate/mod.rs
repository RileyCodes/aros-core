//! Translation engine — trait-based with multiple backends
//!
//! Soniox provides server-side translation via ASR.
//! This module provides local translation fallback and
//! a trait for future backends (ML Kit, local models, etc.)

use std::collections::HashMap;

/// Translation backend trait
pub trait Translator: Send + Sync {
    fn translate(&self, text: &str, from: &str, to: &str) -> Option<String>;
    fn name(&self) -> &str;
    fn is_ready(&self) -> bool;
}

/// Simple dictionary-based translator for common phrases (offline fallback)
pub struct DictionaryTranslator {
    entries: HashMap<String, String>,
}

impl DictionaryTranslator {
    pub fn new_ja_zh() -> Self {
        let mut entries = HashMap::new();
        // Common business/daily phrases
        entries.insert("お疲れ様です".into(), "辛苦了".into());
        entries.insert("ありがとうございます".into(), "谢谢".into());
        entries.insert("すみません".into(), "不好意思".into());
        entries.insert("よろしくお願いします".into(), "请多关照".into());
        entries.insert("了解しました".into(), "明白了".into());
        entries.insert("確認します".into(), "我确认一下".into());
        entries.insert("少々お待ちください".into(), "请稍等".into());
        entries.insert("申し訳ございません".into(), "非常抱歉".into());
        entries.insert("承知しました".into(), "收到".into());
        entries.insert("大丈夫です".into(), "没问题".into());

        Self { entries }
    }

    pub fn lookup(&self, text: &str) -> Option<&str> {
        // Try exact match first
        if let Some(v) = self.entries.get(text) {
            return Some(v);
        }
        // Try trimmed
        let trimmed = text.trim_end_matches(|c| "。！？.!?".contains(c));
        self.entries.get(trimmed).map(|s| s.as_str())
    }
}

impl Translator for DictionaryTranslator {
    fn translate(&self, text: &str, _from: &str, _to: &str) -> Option<String> {
        self.lookup(text).map(|s| s.to_string())
    }

    fn name(&self) -> &str {
        "dictionary-ja-zh"
    }

    fn is_ready(&self) -> bool {
        true
    }
}

/// Placeholder for platform-provided translator (ML Kit on Android, etc.)
pub struct PlatformTranslator;

impl Translator for PlatformTranslator {
    fn translate(&self, _text: &str, _from: &str, _to: &str) -> Option<String> {
        None // Platform implementation calls back to Kotlin
    }

    fn name(&self) -> &str {
        "platform"
    }

    fn is_ready(&self) -> bool {
        false // Set to true when platform reports model downloaded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dictionary_lookup_exact() {
        let dict = DictionaryTranslator::new_ja_zh();
        assert_eq!(dict.lookup("お疲れ様です"), Some("辛苦了"));
        assert_eq!(dict.lookup("ありがとうございます"), Some("谢谢"));
    }

    #[test]
    fn test_dictionary_lookup_with_punctuation() {
        let dict = DictionaryTranslator::new_ja_zh();
        assert_eq!(dict.lookup("お疲れ様です。"), Some("辛苦了"));
        assert_eq!(dict.lookup("確認します！"), Some("我确认一下"));
    }

    #[test]
    fn test_dictionary_lookup_miss() {
        let dict = DictionaryTranslator::new_ja_zh();
        assert_eq!(dict.lookup("今日は天気がいいですね"), None);
    }

    #[test]
    fn test_translator_trait() {
        let dict = DictionaryTranslator::new_ja_zh();
        assert!(dict.is_ready());
        assert_eq!(dict.name(), "dictionary-ja-zh");
        assert_eq!(
            dict.translate("すみません", "ja", "zh"),
            Some("不好意思".to_string())
        );
    }

    #[test]
    fn test_platform_translator_default() {
        let p = PlatformTranslator;
        assert!(!p.is_ready());
        assert_eq!(p.translate("test", "ja", "zh"), None);
    }
}
