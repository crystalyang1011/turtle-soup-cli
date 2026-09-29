//! OpenAI 兼容 LLM 适配层（纯 HTTP，不依赖 Tauri）。
//! 协议与错误映射见 design-doc/02-技术架构.md §3、§4.1 与 07 §4。
// by AI.Coding

use crate::config::AppConfig;
use crate::models::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 单次请求超时（见 02 §3）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 失败后重试次数（指数退避）。
pub const MAX_RETRIES: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

// --- 请求 / 响应 ---

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    temperature: f32,
    stream: bool,
    messages: &'a [ChatMessage],
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Debug, Deserialize)]
struct RespMessage {
    #[serde(default)]
    content: Option<String>,
}

/// LLM 客户端。
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    /// 判定 / 提示使用的模型名。
    pub model: String,
    /// 出题使用的模型名。
    pub author_model: String,
}

impl LlmClient {
    /// 由配置 + API Key 构造。
    pub fn new(cfg: &AppConfig, api_key: impl Into<String>) -> Result<Self, AppError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| AppError::new(ErrorCode::Other, format!("HTTP 客户端初始化失败: {e}")))?;
        Ok(Self {
            http,
            endpoint: cfg.chat_completions_url(),
            api_key: api_key.into(),
            model: cfg.model.clone(),
            author_model: if cfg.host_model.trim().is_empty() {
                cfg.model.clone()
            } else {
                cfg.host_model.clone()
            },
        })
    }

    /// 调用 chat/completions，返回助手文本。带 1 次重试。
    pub async fn complete(
        &self,
        model: &str,
        temperature: f32,
        messages: &[ChatMessage],
    ) -> Result<String, AppError> {
        let mut last_err: Option<AppError> = None;
        for attempt in 0..=MAX_RETRIES {
            match self.complete_once(model, temperature, messages).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    if !e.retryable || attempt == MAX_RETRIES {
                        return Err(e);
                    }
                    // 指数退避：250ms, 500ms...
                    let backoff = Duration::from_millis(250 * (1 << attempt));
                    tokio::time::sleep(backoff).await;
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::new(ErrorCode::Other, "未知网络错误")))
    }

    async fn complete_once(
        &self,
        model: &str,
        temperature: f32,
        messages: &[ChatMessage],
    ) -> Result<String, AppError> {
        let started = std::time::Instant::now();
        let body = ChatRequest { model, temperature, stream: false, messages };
        let resp = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let err = map_reqwest_err(e);
                crate::log_error!("llm", "请求失败 model={model}: {err}");
                err
            })?;

        let status = resp.status();
        let status_u16 = status.as_u16();
        // 读出整段文本，先记日志再解析（解析失败时原始响应是唯一线索，见 07 §7）。
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("响应读取失败: {e}")))?;
        let elapsed = started.elapsed().as_millis();

        if !status.is_success() {
            crate::logging::llm_call(model, elapsed, Some(status_u16), Some(&text));
            return Err(map_status(status_u16, &text));
        }
        crate::logging::llm_call(model, elapsed, Some(status_u16), Some(&text));

        let parsed: ChatResponse = serde_json::from_str(&text)
            .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("响应解析失败: {e}")))?;

        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| AppError::new(ErrorCode::ParseFailed, "模型返回内容为空"))
    }

    /// 连通性自测：发一条最小请求（见 01 §10 首启引导）。
    pub async fn test_connection(&self) -> Result<(), AppError> {
        let msgs = [ChatMessage::user("ping")];
        self.complete(&self.model, 0.0, &msgs).await.map(|_| ())
    }
}

/// reqwest 错误 → 错误码。
fn map_reqwest_err(e: reqwest::Error) -> AppError {
    if e.is_timeout() {
        AppError::new(ErrorCode::Timeout, "请求超时（20s）")
    } else if e.is_connect() {
        AppError::new(ErrorCode::NetworkError, "网络连接失败")
    } else if e.is_decode() {
        AppError::new(ErrorCode::ParseFailed, format!("响应解码失败: {e}"))
    } else {
        AppError::new(ErrorCode::NetworkError, format!("请求失败: {e}"))
    }
}

/// HTTP 状态码 → 错误码（见 02 §4.1）。
fn map_status(code: u16, body: &str) -> AppError {
    let snippet: String = body.chars().take(200).collect();
    let err_code = match code {
        401 | 403 => ErrorCode::AuthFailed,
        402 => ErrorCode::QuotaExceeded,
        429 => ErrorCode::RateLimited,
        400 => ErrorCode::ParseFailed,
        c if c >= 500 => ErrorCode::NetworkError,
        _ => ErrorCode::Other,
    };
    AppError::new(err_code, format!("HTTP {code}: {snippet}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(map_status(401, "").code, ErrorCode::AuthFailed);
        assert_eq!(map_status(402, "").code, ErrorCode::QuotaExceeded);
        assert_eq!(map_status(429, "").code, ErrorCode::RateLimited);
        assert_eq!(map_status(500, "").code, ErrorCode::NetworkError);
        assert!(map_status(429, "").retryable);
        assert!(!map_status(401, "").retryable);
    }
}
