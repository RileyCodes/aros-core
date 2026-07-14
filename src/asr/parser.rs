//! Soniox WebSocket response parser

use serde_json::Value;

/// Parsed result from a single Soniox WebSocket message
#[derive(Debug, Default)]
pub struct ParsedTokens {
    pub finalized: String,
    pub partial: String,
    pub translation: String,
    pub speaker: i32,
    pub has_partial: bool,
}

/// Parse a Soniox WebSocket JSON response into structured tokens
pub fn parse_soniox_response(json_str: &str) -> Result<ParsedTokens, serde_json::Error> {
    let json: Value = serde_json::from_str(json_str)?;

    // Check for error
    if json.get("error_code").is_some() {
        let code = json["error_code"].as_str().unwrap_or("unknown");
        let msg = json["error_message"].as_str().unwrap_or("");
        log::error!("Soniox error: code={} msg={}", code, msg);
        return Ok(ParsedTokens::default());
    }

    let tokens = match json.get("tokens").and_then(|t| t.as_array()) {
        Some(t) => t,
        None => return Ok(ParsedTokens::default()),
    };

    let mut result = ParsedTokens {
        speaker: -1,
        ..Default::default()
    };

    for token in tokens {
        let text = token["text"].as_str().unwrap_or("");
        // Skip Soniox control tokens
        if text == "<fin>" || text == "<end>" {
            continue;
        }
        let is_final = token["is_final"].as_bool().unwrap_or(false);
        let is_translation =
            token.get("translation_status").and_then(|v| v.as_str()) == Some("translation");
        let speaker = token["speaker"].as_i64().unwrap_or(-1) as i32;

        if speaker >= 0 {
            result.speaker = speaker;
        }

        if is_translation {
            result.translation.push_str(text);
        } else if is_final {
            result.finalized.push_str(text);
        } else {
            result.partial.push_str(text);
            result.has_partial = true;
        }
    }

    Ok(result)
}

/// Strip reading annotations like 確認(かくにん) → 確認
pub fn strip_readings(text: &str) -> String {
    let re = regex_lite::Regex::new(r"\([ぁ-ゖー]+\)").unwrap();
    re.replace_all(text, "").to_string()
}

/// Check if text contains Japanese (hiragana or katakana)
pub fn is_japanese(text: &str) -> bool {
    text.chars()
        .any(|c| ('\u{3040}'..='\u{309F}').contains(&c) || ('\u{30A0}'..='\u{30FF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_final_token() {
        let json = r#"{"tokens":[{"text":"今日は","is_final":true,"speaker":0}]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "今日は");
        assert_eq!(result.partial, "");
        assert_eq!(result.speaker, 0);
        assert!(!result.has_partial);
    }

    #[test]
    fn test_parse_partial_token() {
        let json = r#"{"tokens":[{"text":"天気が","is_final":false}]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "");
        assert_eq!(result.partial, "天気が");
        assert!(result.has_partial);
    }

    #[test]
    fn test_parse_translation() {
        let json = r#"{"tokens":[
            {"text":"今日は天気がいいですね。","is_final":true,"speaker":0},
            {"text":"今天天气真好。","translation_status":"translation"}
        ]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "今日は天気がいいですね。");
        assert_eq!(result.translation, "今天天气真好。");
    }

    #[test]
    fn test_parse_mixed_tokens() {
        let json = r#"{"tokens":[
            {"text":"確認","is_final":true,"speaker":1},
            {"text":"しま","is_final":false},
            {"text":"确认","translation_status":"translation"}
        ]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "確認");
        assert_eq!(result.partial, "しま");
        assert_eq!(result.translation, "确认");
        assert_eq!(result.speaker, 1);
        assert!(result.has_partial);
    }

    #[test]
    fn test_parse_error_response() {
        let json = r#"{"error_code":"invalid_api_key","error_message":"bad key"}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "");
    }

    #[test]
    fn test_parse_empty_tokens() {
        let json = r#"{"tokens":[]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "");
        assert!(!result.has_partial);
    }

    #[test]
    fn test_parse_no_tokens_field() {
        let json = r#"{"status":"ok"}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "");
    }

    #[test]
    fn test_strip_readings() {
        assert_eq!(strip_readings("確認(かくにん)します"), "確認します");
        assert_eq!(strip_readings("天気(てんき)がいい"), "天気がいい");
        assert_eq!(strip_readings("hello"), "hello");
    }

    #[test]
    fn test_is_japanese() {
        assert!(is_japanese("今日はいい天気です"));
        assert!(is_japanese("カタカナ"));
        assert!(is_japanese("mixed English and にほんご"));
        assert!(!is_japanese("hello world"));
        assert!(!is_japanese("你好世界"));
        assert!(!is_japanese("123"));
    }

    #[test]
    fn test_multiple_speakers() {
        let json = r#"{"tokens":[
            {"text":"はい。","is_final":true,"speaker":0},
            {"text":"そうですね。","is_final":true,"speaker":1}
        ]}"#;
        let result = parse_soniox_response(json).unwrap();
        assert_eq!(result.finalized, "はい。そうですね。");
        assert_eq!(result.speaker, 1); // Last speaker wins
    }
}
