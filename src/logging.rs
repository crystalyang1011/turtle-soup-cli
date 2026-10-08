//! 本地日志：落到项目根 `data/logs/app.log`，诊断判定/网络异常。
//! 见 docs/1-turtle-cli/07-安全与异常.md §7。
//! **绝不写入 api_key / 汤底**；LLM 原始响应按需截断记录（解析失败时是关键线索）。
// by AI.Coding

use crate::models::now_ts;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

/// 进程内日志文件路径（首次使用时初始化）。
static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 是否输出 debug 级（`TURTLE_LOG=debug`）。
fn debug_enabled() -> bool {
    std::env::var("TURTLE_LOG")
        .map(|v| v.eq_ignore_ascii_case("debug"))
        .unwrap_or(false)
}

/// 日志文件路径（可能为 None：无法定位数据目录）。
fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH
        .get_or_init(|| {
            let dir = crate::data_dir().ok()?.join("logs");
            std::fs::create_dir_all(&dir).ok()?;
            Some(dir.join("app.log"))
        })
        .as_ref()
}

/// 当前日志文件路径（供 UI/CLI 提示用户）。
pub fn current_log_path() -> Option<PathBuf> {
    log_path().cloned()
}

/// 时间戳（本地简化格式，仅用于日志可读性）。
fn ts() -> String {
    let secs = now_ts();
    // 转成 HH:MM:SS（UTC，够用；不引入额外依赖）。
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// 写一行日志。失败静默（日志系统不能反过来影响游戏）。
pub fn log(level: Level, module: &str, msg: &str) {
    if level == Level::Debug && !debug_enabled() {
        return;
    }
    // 简单脱敏：防呆，避免调用方误传 key。
    let safe = redact(msg);
    let line = format!("{} {} [{}] {}\n", ts(), level.tag(), module, safe);
    if let Some(path) = log_path() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
    // DEBUG 之外也在 stderr 打警告/错误，便于 CLI 即时可见。
    if level >= Level::Warn {
        eprint!("{line}");
    }
}

/// 记录 LLM 调用（含耗时与原始响应前 500 字）。见 07 §7 必记内容。
pub fn llm_call(model: &str, elapsed_ms: u128, http_status: Option<u16>, raw: Option<&str>) {
    let preview = raw
        .map(|r| r.chars().take(500).collect::<String>())
        .unwrap_or_default();
    log(
        if http_status.map(|s| s >= 400).unwrap_or(false) {
            Level::Error
        } else {
            Level::Debug
        },
        "llm",
        &format!(
            "model={model} elapsed={elapsed_ms}ms status={} resp={preview:?}",
            http_status.map(|s| s.to_string()).unwrap_or_else(|| "-".into())
        ),
    );
}

/// 兜底脱敏：把形如 `sk-...` / `ark-...` 的长 token 打码。
fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for token in s.split_inclusive(|c: char| c.is_whitespace()) {
        let trimmed = token.trim_end();
        let looks_key = (trimmed.starts_with("sk-")
            || trimmed.starts_with("ark-")
            || trimmed.starts_with("Bearer "))
            && trimmed.len() > 12;
        if looks_key {
            out.push_str(&crate::secrets::mask(trimmed));
            out.push_str(&token[trimmed.len()..]);
        } else {
            out.push_str(token);
        }
    }
    out
}

/// 便捷宏：`log_info!("engine", "...")`。
#[macro_export]
macro_rules! log_error {
    ($m:expr, $($arg:tt)*) => { $crate::logging::log($crate::logging::Level::Error, $m, &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($m:expr, $($arg:tt)*) => { $crate::logging::log($crate::logging::Level::Warn, $m, &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_info {
    ($m:expr, $($arg:tt)*) => { $crate::logging::log($crate::logging::Level::Info, $m, &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_debug {
    ($m:expr, $($arg:tt)*) => { $crate::logging::log($crate::logging::Level::Debug, $m, &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_masks_keys() {
        let s = redact("call with sk-1234567890abcdef and ark-abcdef1234567890 tail");
        assert!(!s.contains("sk-1234567890abcdef"));
        assert!(!s.contains("ark-abcdef1234567890"));
        assert!(s.contains("tail"));
    }

    #[test]
    fn redact_keeps_normal_text() {
        assert_eq!(redact("男人在打嗝"), "男人在打嗝");
    }
}
