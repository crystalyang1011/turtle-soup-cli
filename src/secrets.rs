//! API Key 解析。来源优先级（见 docs/1-turtle-cli/07 §3）：
//! 环境变量 `TURTLE_API_KEY` > `config.json` 内联 `api_key`。
//! v3.0 起只有 CLI，不再使用 OS 凭据库（keyring 依赖已移除）。
//! 明文只允许出现在本地 gitignore 的 config.json 中，绝不进 git/日志/终端回显。
// by AI.Coding

use crate::config::AppConfig;
use crate::models::{AppError, ErrorCode};

/// 环境变量名。
pub const ENV_API_KEY: &str = "TURTLE_API_KEY";

/// 解析当前生效的 API Key：env > config 内联。
pub fn resolve(cfg: &AppConfig) -> Result<String, AppError> {
    if let Ok(k) = std::env::var(ENV_API_KEY) {
        if !k.trim().is_empty() {
            return Ok(k);
        }
    }
    if !cfg.api_key.trim().is_empty() {
        return Ok(cfg.api_key.clone());
    }
    Err(AppError::new(
        ErrorCode::NoApiKey,
        format!(
            "未找到 API Key：请设置环境变量 {ENV_API_KEY}，或在 config.json 里填 api_key"
        ),
    ))
}

/// 脱敏展示（日志/错误用，见 07 §3）。
pub fn mask(key: &str) -> String {
    let n = key.len();
    if n <= 8 {
        return "****".to_string();
    }
    format!("{}****{}", &key[..4], &key[n - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_middle() {
        assert_eq!(mask("sk-1234567890abcd"), "sk-1****abcd");
        assert_eq!(mask("short"), "****");
    }

    #[test]
    fn inline_key_takes_priority_when_no_env() {
        // 确保测试环境没有污染的环境变量。
        std::env::remove_var(ENV_API_KEY);
        let cfg = AppConfig { api_key: "inline-key".into(), ..Default::default() };
        assert_eq!(resolve(&cfg).unwrap(), "inline-key");
    }

    #[test]
    fn missing_key_is_no_api_key_error() {
        std::env::remove_var(ENV_API_KEY);
        let cfg = AppConfig::default();
        assert_eq!(resolve(&cfg).unwrap_err().code, ErrorCode::NoApiKey);
    }
}
