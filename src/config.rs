//! 配置加载与校验。见 docs/1-turtle-cli/02-技术架构.md §3、07 §3、03 §4。
//! 配置放工作目录根的 `config.json`（`.env` 理念，允许内联 Key，已 gitignore）。
// by AI.Coding

use crate::models::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 配置。磁盘为 snake_case，`base_url` 兼容 `baseUrl`。
/// `api_key` 允许内联（个人单机自用，`.env` 理念）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(alias = "baseUrl")]
    pub base_url: String,
    /// 允许内联的 API Key（`.env` 理念）。留空则改用环境变量 TURTLE_API_KEY。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub host_model: String,
}

impl AppConfig {
    /// 候选配置路径（按优先级）：TURTLE_CONFIG → 当前工作目录 → 项目根 → 项目根 data/。
    pub fn candidate_paths() -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Ok(p) = std::env::var("TURTLE_CONFIG") {
            if !p.trim().is_empty() {
                v.push(PathBuf::from(p));
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join("config.json"));
        }
        v.push(crate::project_root().join("config.json"));
        if let Ok(p) = Self::data_path() {
            v.push(p);
        }
        v
    }

    /// 项目根 `data/` 下的配置路径（兜底）。
    pub fn data_path() -> Result<PathBuf, AppError> {
        Ok(crate::data_dir()?.join("config.json"))
    }

    /// 实际解析到的配置路径：第一个存在的文件；都不存在则用工作目录根。
    pub fn resolved_path() -> PathBuf {
        Self::candidate_paths()
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|c| c.join("config.json"))
                    .unwrap_or_else(|_| PathBuf::from("config.json"))
            })
    }

    /// 从指定路径加载并校验。
    pub fn load_from(path: &Path) -> Result<Self, AppError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            AppError::new(
                ErrorCode::InvalidConfig,
                format!("无法读取配置文件 {}：{e}", path.display()),
            )
        })?;
        let cfg: AppConfig = serde_json::from_str(&text).map_err(|e| {
            AppError::new(
                ErrorCode::InvalidConfig,
                format!("配置文件 {} 不是合法 JSON：{e}", path.display()),
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 按候选顺序加载第一个存在的配置；都不存在返回默认值。
    pub fn load_or_default() -> Result<Self, AppError> {
        for p in Self::candidate_paths() {
            if p.exists() {
                return Self::load_from(&p);
            }
        }
        Ok(AppConfig::default())
    }

    /// 原子写入解析到的路径（可能含内联 Key）。
    pub fn save(&self) -> Result<(), AppError> {
        crate::session::write_atomic_json(&Self::resolved_path(), self)
    }

    /// 校验：字段可读报错。`base_url` 必填且须 http(s)。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.base_url.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                "缺少 base_url：请在设置页或 config.json 填写模型服务地址",
            ));
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                format!("base_url 非法（须以 http:// 或 https:// 开头）：{}", self.base_url),
            ));
        }
        if self.model.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                "缺少 model：请在设置页或 config.json 填写判定模型名",
            ));
        }
        Ok(())
    }

    /// 主持人模型（未单独配置时回落到 `model`）。
    pub fn effective_host_model(&self) -> &str {
        if self.host_model.trim().is_empty() {
            &self.model
        } else {
            &self.host_model
        }
    }

    /// 拼接 chat/completions 端点。
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_missing_and_bad_url() {
        // 缺 base_url
        let missing_url = AppConfig { model: "m".into(), ..Default::default() };
        assert_eq!(missing_url.validate().unwrap_err().code, ErrorCode::InvalidConfig);

        // 非 http(s)
        let bad = AppConfig {
            base_url: "ftp://x".into(),
            model: "m".into(),
            ..Default::default()
        };
        assert!(bad.validate().is_err());

        // 合法
        let ok = AppConfig {
            base_url: "https://example.com".into(),
            model: "m".into(),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn url_join_and_host_fallback() {
        let c = AppConfig {
            base_url: "https://api.example.com/v1/".into(),
            model: "cheap".into(),
            ..Default::default()
        };
        assert_eq!(c.chat_completions_url(), "https://api.example.com/v1/chat/completions");
        assert_eq!(c.effective_host_model(), "cheap");
    }

    #[test]
    fn load_from_missing_file_is_invalid_config() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nope.json");
        assert_eq!(
            AppConfig::load_from(&p).unwrap_err().code,
            ErrorCode::InvalidConfig
        );
    }

    #[test]
    fn inline_api_key_roundtrips_json() {
        let c = AppConfig {
            base_url: "https://x".into(),
            api_key: "sk-abc".into(),
            model: "m".into(),
            ..Default::default()
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"api_key\":\"sk-abc\""));
        let back: AppConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.api_key, "sk-abc");
    }

    #[test]
    fn env_key_takes_priority_over_inline() {
        // 环境变量优先级高于 config 内联（用独立 key 避免并发串扰）。
        let key = "TURTLE_API_KEY";
        let cfg = AppConfig { api_key: "inline".into(), ..Default::default() };
        std::env::set_var(key, "from-env");
        assert_eq!(crate::secrets::resolve(&cfg).unwrap(), "from-env");
        std::env::remove_var(key);
        assert_eq!(crate::secrets::resolve(&cfg).unwrap(), "inline");
    }
}
