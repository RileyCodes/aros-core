//! AI suggestion engine — OpenRouter streaming LLM requests

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub text: String,
    pub translation: Option<String>, // Chinese translation
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionPack {
    pub mood: Option<String>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Clone)]
pub struct AiEngine {
    api_key: String,
    client: Client,
    model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("No update needed")]
    NoUpdate,
}

impl AiEngine {
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn new(api_key: &str) -> Self {
        // Install ring crypto provider (aws-lc-rs hangs on Android aarch64)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Configure rustls with webpki root certificates
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Self {
            api_key: api_key.to_string(),
            client: Client::builder()
                .use_preconfigured_tls(tls_config)
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .pool_idle_timeout(std::time::Duration::from_secs(10))
                .pool_max_idle_per_host(1)
                .build()
                .expect("HTTP client"),
            model: "anthropic/claude-sonnet-4.6".to_string(),
        }
    }

    /// Generate conversation suggestions based on history
    pub async fn suggest(
        &self,
        history: &[String],
        memory: &str,
        guide: Option<&crate::memory::GuideData>,
    ) -> Result<SuggestionPack, AiError> {
        let context = history.join("\n");
        let mem_note = if memory.is_empty() {
            String::new()
        } else {
            format!(
                "\n【記憶】\n{}\n\
                 ※ 記憶は背景情報。現在の会話または指導モードに明確に関係する場合だけ使うこと。\n\
                 ※ 関係しない診療・薬・予約・個人情報を提案に混ぜないこと。",
                memory
            )
        };

        let guide_note = if let Some(g) = guide {
            format!(
                "\n【指導モード】目標: {}\n{}\n\
                 ⚠️ 提案はこの目標に沿った内容を優先すること。",
                g.goal, g.context
            )
        } else {
            String::new()
        };

        let prompt = format!(
            "会話アシスタント。ユーザー(日本語学習者)が次に言うべき日本語の返答を4つ提案。\n\
             会話履歴の P1/P2 は Soniox の話者分離。必ず別人として扱い、P1 と P2 の発言を混ぜない。\n\
             最初に中国語で极短冲突/愤怒风险を1行だけ出す。普通の雰囲気要約ではなく、衝突検知を優先する。\n\
             形式：情绪: 低/中/高 P1怒/P2怒/双怒/无怒 安抚/确认/转移/退出\n\
             その後に4つ提案。各提案：日本語(ふりがな付き) | 中文翻译\n\
             {mem_note}{guide_note}\n\
             【会話履歴】\n{context}\n\n\
             ■ ルール：\n\
             - 1行目は必ず「情绪: ...」。10字以内を目標。废话禁止，不能写“策略/怒气/冲突”等重复字段名\n\
             - 冲突高: 责备、威胁、拒绝、反复打断、明显不满、命令压迫、讽刺升级\n\
             - 冲突中: 分歧、犹豫、推拉、价格/责任/时间谈判、轻微防御\n\
             - 怒气は P1/P2 を分ける。明显一方不满写 P1怒 或 P2怒，双方写 双怒，不明显写 无怒\n\
             - 推荐回复は冲突レベルに合わせる。高なら先降温、确认事实、避免反击\n\
             - 番号付き、1行に日本語と中文を | で区切る\n\
             - 丁寧語で短く\n\n\
             - 記憶が会話と無関係なら完全に無視する\n\
             - 医療・予約・薬の話題は、会話または指導モードに出ている時だけ扱う\n\n\
             例:\n\
             情绪: 中 P2怒 安抚\n\
             1. 確認(かくにん)します | 我确认一下\n\
             2. 承知(しょうち)しました | 了解了"
        );

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 200,
            "messages": [{"role": "user", "content": prompt}]
        });

        let start = Instant::now();
        let resp = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        let elapsed = start.elapsed();
        log::info!(
            "AI suggest: HTTP {} in {}ms, {} bytes",
            status,
            elapsed.as_millis(),
            text.len()
        );

        if !status.is_success() {
            return Err(AiError::Parse(format!(
                "HTTP {}: {}",
                status,
                &text[..200.min(text.len())]
            )));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(e.to_string()))?;

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");

        parse_suggestion_pack(content)
    }

    /// Update memory via LLM
    pub async fn update_memory(
        &self,
        current_memory: &str,
        requests: &[String],
    ) -> Result<String, AiError> {
        let req_list: String = requests
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "你是记忆管理助手。根据用户的语音指令更新记忆。\n\n\
             当前记忆：\n{current}\n\n\
             用户修改请求：\n{reqs}\n\n\
             规则：\n\
             - 合并用户请求到现有记忆中\n\
             - 如果用户说删除/去掉某内容，从记忆中移除\n\
             - 如果用户说修改/改成，替换对应内容\n\
             - 如果用户说添加/加上，追加到记忆\n\
             - 保持简洁，用关键词和短句\n\n\
             只返回更新后的记忆内容，不要任何其他文字。",
            current = if current_memory.is_empty() {
                "(空)"
            } else {
                current_memory
            },
            reqs = req_list
        );

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 300,
            "messages": [{"role": "user", "content": prompt}]
        });

        let resp = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let text = resp.text().await?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(e.to_string()))?;

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        Ok(content)
    }
}

/// Parse mood + numbered suggestion lines from LLM response.
pub fn parse_suggestion_pack(content: &str) -> Result<SuggestionPack, AiError> {
    let mood = content
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("情绪:")
                .or_else(|| line.strip_prefix("情緒:"))
                .or_else(|| line.strip_prefix("Mood:"))
                .or_else(|| line.strip_prefix("mood:"))
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty());
    let suggestions = parse_suggestions(content)?;
    Ok(SuggestionPack { mood, suggestions })
}

/// Parse numbered suggestion lines from LLM response
pub fn parse_suggestions(content: &str) -> Result<Vec<Suggestion>, AiError> {
    if content.contains("NO_UPDATE") {
        return Err(AiError::NoUpdate);
    }

    let items: Vec<Suggestion> = content
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            trimmed.len() > 2
                && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
                && trimmed.chars().nth(1) == Some('.')
        })
        .take(4)
        .map(|l| {
            let raw = l
                .trim()
                .replacen(|c: char| c.is_ascii_digit() || c == '.', "", 2)
                .trim()
                .to_string();
            // Split "日本語 | 中文" format
            let (text, translation) = if let Some(idx) = raw.find('|') {
                (
                    raw[..idx].trim().to_string(),
                    Some(raw[idx + 1..].trim().to_string()),
                )
            } else {
                (raw, None)
            };
            Suggestion { translation, text }
        })
        .collect();

    if items.is_empty() {
        Err(AiError::Parse("No numbered suggestions found".to_string()))
    } else {
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_suggestions_basic() {
        let content = "1. お疲れ様(おつかれさま)です\n2. ありがとうございます\n3. 確認(かくにん)します\n4. 了解(りょうかい)しました";
        let items = parse_suggestions(content).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].text, "お疲れ様(おつかれさま)です");
        assert_eq!(items[3].text, "了解(りょうかい)しました");
    }

    #[test]
    fn test_parse_suggestions_with_translation() {
        let content = "1. 確認(かくにん)します | 我确认一下\n2. 承知(しょうち)しました | 了解了\n3. はい | 好的\n4. ありがとう | 谢谢";
        let items = parse_suggestions(content).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].text, "確認(かくにん)します");
        assert_eq!(items[0].translation, Some("我确认一下".to_string()));
        assert_eq!(items[2].text, "はい");
        assert_eq!(items[2].translation, Some("好的".to_string()));
    }

    #[test]
    fn test_parse_suggestions_no_translation() {
        let content = "1. お疲れ様です\n2. ありがとう";
        let items = parse_suggestions(content).unwrap();
        assert_eq!(items[0].translation, None);
    }

    #[test]
    fn test_parse_suggestions_no_update() {
        let content = "NO_UPDATE";
        assert!(matches!(parse_suggestions(content), Err(AiError::NoUpdate)));
    }

    #[test]
    fn test_parse_suggestions_mixed_content() {
        let content = "Here are suggestions:\n1. はい\n2. いいえ\nSome trailing text";
        let items = parse_suggestions(content).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn test_parse_suggestions_empty() {
        let content = "I don't understand";
        assert!(parse_suggestions(content).is_err());
    }

    #[test]
    fn test_parse_max_four() {
        let content = "1. a\n2. b\n3. c\n4. d\n5. e\n6. f";
        let items = parse_suggestions(content).unwrap();
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn test_parse_suggestion_pack_with_mood() {
        let content = "情绪: 中 P2怒 安抚\n1. 確認(かくにん)します | 我确认一下\n2. はい | 好的";
        let pack = parse_suggestion_pack(content).unwrap();
        assert_eq!(pack.mood, Some("中 P2怒 安抚".to_string()));
        assert_eq!(pack.suggestions.len(), 2);
        assert_eq!(
            pack.suggestions[0].translation,
            Some("我确认一下".to_string())
        );
    }
}
