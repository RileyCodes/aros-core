//! Voice command parser — detects trigger phrases in ASR text

/// Recognized voice commands
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    EnterDialogue,   // "克劳德" / "Claude"
    ExitDialogue,    // "结束对话" / "終了"
    SessionReset,    // "OK 重置"
    ForceSuggest,    // "建议" / "提案"
    ToggleTts,       // "静音" / "ミュート"
    RepeatLast,      // "重复" / "もう一度"
    TakePhoto,       // "写真" / "拍照" / "photo"
    ShowStatus,      // "ステータス" / "状态" / "status"
    ClearMemory,     // "記憶を消して" / "清除记忆" / "clear memory"
    ShowHelp,        // "使用说明" / "ヘルプ" / "help"
    ToggleGuide,     // "指导模式" / "ガイドモード" / "guide mode"
    OpenSettings,    // "设置" / "設定" / "settings"
    EnterGeminiLive, // "ジェミニ" / "Gemini" / "杰米尼"
}

pub struct CommandParser {
    // Extensible: future commands can have configurable triggers
}

impl CommandParser {
    pub fn new() -> Self {
        Self {}
    }

    /// Detect a voice command in the given text.
    /// `mode` affects which commands are active (e.g. "确认" only in memory edit mode).
    pub fn detect(&self, text: &str) -> Option<Command> {
        self.detect_in_mode(text, "listening")
    }

    pub fn detect_in_mode(&self, text: &str, mode: &str) -> Option<Command> {
        let lower = text.to_lowercase();
        let lower = lower.trim();

        // All commands require "克劳德" prefix (or ASR variants クロード/プロード)
        // This prevents ALL accidental activations
        let has_prefix = lower.contains("克劳德")
            || lower.contains("クロード")
            || lower.contains("プロード")
            || lower.contains("くろーど")
            || lower.contains("claude");

        // === Mode-specific commands ===

        // In gemini_live mode: only exit
        if mode == "gemini_live" {
            if has_prefix && (lower.contains("结束") || lower.contains("終了")) {
                log::info!("CMD: ExitDialogue (from gemini_live) in \"{}\"", text);
                return Some(Command::ExitDialogue);
            }
            return None;
        }

        // In dialogue mode: exit + utility commands (all require prefix)
        if mode == "dialogue" {
            if has_prefix && (lower.contains("结束") || lower.contains("終了")) {
                log::info!("CMD: ExitDialogue in \"{}\"", text);
                return Some(Command::ExitDialogue);
            }
            if has_prefix && lower.contains("静音") {
                log::info!("CMD: ToggleTts in \"{}\"", text);
                return Some(Command::ToggleTts);
            }
            if has_prefix && lower.contains("拍照") {
                log::info!("CMD: TakePhoto in \"{}\"", text);
                return Some(Command::TakePhoto);
            }
            if has_prefix && lower.contains("重复") {
                log::info!("CMD: RepeatLast in \"{}\"", text);
                return Some(Command::RepeatLast);
            }
            return None;
        }

        // === Global commands (listening mode) — all require 克劳德 prefix ===

        if !has_prefix {
            return None;
        }

        // Enter dialogue — "克劳德" alone or "克劳德" + question
        // Check specific sub-commands first, then fall through to dialogue
        if lower.contains("语音助手") {
            log::info!("CMD: EnterGeminiLive in \"{}\"", text);
            return Some(Command::EnterGeminiLive);
        }
        if lower.contains("状态") {
            log::info!("CMD: ShowStatus in \"{}\"", text);
            return Some(Command::ShowStatus);
        }
        if lower.contains("帮助") || lower.contains("使用说明") {
            log::info!("CMD: ShowHelp in \"{}\"", text);
            return Some(Command::ShowHelp);
        }
        if lower.contains("指导模式") {
            log::info!("CMD: ToggleGuide in \"{}\"", text);
            return Some(Command::ToggleGuide);
        }
        if lower.contains("设置") {
            log::info!("CMD: OpenSettings in \"{}\"", text);
            return Some(Command::OpenSettings);
        }
        if lower.contains("清除记忆") {
            log::info!("CMD: ClearMemory in \"{}\"", text);
            return Some(Command::ClearMemory);
        }
        if lower.contains("重置") {
            log::info!("CMD: SessionReset in \"{}\"", text);
            return Some(Command::SessionReset);
        }
        if lower.contains("建议") {
            log::info!("CMD: ForceSuggest in \"{}\"", text);
            return Some(Command::ForceSuggest);
        }
        if lower.contains("静音") {
            log::info!("CMD: ToggleTts in \"{}\"", text);
            return Some(Command::ToggleTts);
        }
        if lower.contains("拍照") {
            log::info!("CMD: TakePhoto in \"{}\"", text);
            return Some(Command::TakePhoto);
        }
        if lower.contains("重复") {
            log::info!("CMD: RepeatLast in \"{}\"", text);
            return Some(Command::RepeatLast);
        }
        if lower.contains("结束") || lower.contains("終了") {
            log::info!("CMD: ExitDialogue in \"{}\"", text);
            return Some(Command::ExitDialogue);
        }

        // Default: "克劳德" + anything else = enter dialogue
        log::info!("CMD: EnterDialogue in \"{}\"", text);
        Some(Command::EnterDialogue)
    }
}

impl Default for CommandParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === All commands require 克劳德 prefix ===

    #[test]
    fn test_enter_dialogue() {
        let p = CommandParser::new();
        assert_eq!(
            p.detect("克劳德，今天天气怎么样？"),
            Some(Command::EnterDialogue)
        );
        assert_eq!(p.detect("クロード、天気は？"), Some(Command::EnterDialogue));
        assert_eq!(p.detect("プロード、何？"), Some(Command::EnterDialogue));
        assert_eq!(p.detect("Claude, hello"), Some(Command::EnterDialogue));
        // Just "克劳德" alone = enter dialogue
        assert_eq!(p.detect("克劳德"), Some(Command::EnterDialogue));
    }

    #[test]
    fn test_sub_commands() {
        let p = CommandParser::new();
        assert_eq!(p.detect("克劳德清除记忆"), Some(Command::ClearMemory));
        assert_eq!(p.detect("克劳德重置"), Some(Command::SessionReset));
        assert_eq!(p.detect("克劳德建议"), Some(Command::ForceSuggest));
        assert_eq!(p.detect("克劳德静音"), Some(Command::ToggleTts));
        assert_eq!(p.detect("克劳德拍照"), Some(Command::TakePhoto));
        assert_eq!(p.detect("克劳德重复"), Some(Command::RepeatLast));
        assert_eq!(p.detect("克劳德状态"), Some(Command::ShowStatus));
        assert_eq!(p.detect("克劳德帮助"), Some(Command::ShowHelp));
        assert_eq!(p.detect("克劳德设置"), Some(Command::OpenSettings));
        assert_eq!(p.detect("克劳德指导模式"), Some(Command::ToggleGuide));
        assert_eq!(p.detect("克劳德语音助手"), Some(Command::EnterGeminiLive));
        assert_eq!(p.detect("克劳德结束"), Some(Command::ExitDialogue));
    }

    #[test]
    fn test_no_prefix_no_command() {
        let p = CommandParser::new();
        // Without 克劳德 prefix, nothing triggers
        assert_eq!(p.detect("今日は天気がいいですね。"), None);
        assert_eq!(p.detect("hello world"), None);
        assert_eq!(p.detect("help"), None);
        assert_eq!(p.detect("status"), None);
        assert_eq!(p.detect("settings"), None);
        assert_eq!(p.detect("拍照"), None);
        assert_eq!(p.detect("静音"), None);
        assert_eq!(p.detect("建议"), None);
        assert_eq!(p.detect(""), None);
    }

    // === Dialogue mode ===

    #[test]
    fn test_exit_dialogue() {
        let p = CommandParser::new();
        assert_eq!(
            p.detect_in_mode("克劳德结束", "dialogue"),
            Some(Command::ExitDialogue)
        );
        assert_eq!(
            p.detect_in_mode("クロード終了", "dialogue"),
            Some(Command::ExitDialogue)
        );
    }

    #[test]
    fn test_dialogue_requires_prefix() {
        let p = CommandParser::new();
        // Without prefix, nothing triggers in dialogue
        assert_eq!(p.detect_in_mode("结束", "dialogue"), None);
        assert_eq!(p.detect_in_mode("静音", "dialogue"), None);
        assert_eq!(p.detect_in_mode("拍照", "dialogue"), None);
    }

    #[test]
    fn test_dialogue_allows_with_prefix() {
        let p = CommandParser::new();
        assert_eq!(
            p.detect_in_mode("克劳德静音", "dialogue"),
            Some(Command::ToggleTts)
        );
        assert_eq!(
            p.detect_in_mode("克劳德拍照", "dialogue"),
            Some(Command::TakePhoto)
        );
        assert_eq!(
            p.detect_in_mode("克劳德重复", "dialogue"),
            Some(Command::RepeatLast)
        );
    }
}
