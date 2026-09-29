//! 海龟汤摸鱼游戏 · CLI 版核心库。
//!
//! 纯 Rust，无 GUI：`engine` / `session` / `llm` / `dataset` / `game` 全部可测，
//! 由 `soup-cli` 二进制驱动（见 design-doc/02-技术架构.md）。
// by AI.Coding

pub mod config;
pub mod dataset;
pub mod engine;
pub mod game;
pub mod llm;
pub mod logging;
pub mod models;
pub mod prompts;
pub mod secrets;
pub mod session;

pub use models::AppError;
pub use models::ErrorCode;

/// 当前 schema 版本（puzzles.json / sessions/*.json / stats.json）。
pub const SCHEMA_VERSION: u32 = 1;

/// 应用数据目录名（`%APPDATA%/turtle-soup`）。
pub const APP_DIR_NAME: &str = "turtle-soup";

/// 返回应用数据目录（不存在则创建）。
pub fn app_data_dir() -> Result<std::path::PathBuf, AppError> {
    let dirs = directories::ProjectDirs::from("", "", APP_DIR_NAME)
        .ok_or_else(|| AppError::new(ErrorCode::Io, "无法定位应用数据目录"))?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 项目根目录（**运行时**解析，容忍仓库被移动 / 改名，见 02 §4.3）。
///
/// 候选来源按优先级：编译期 crate 根（`CARGO_MANIFEST_DIR`）→ 当前工作目录 →
/// 可执行文件所在目录，各候选再连同其向上若干级祖先一并探测；
/// 命中含 `assets/puzzles.json` 或 `Cargo.toml` 的目录即视为根。
/// 全部落空时退化到可执行文件目录。
pub fn project_root() -> std::path::PathBuf {
    let mut seeds: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    if let Ok(cwd) = std::env::current_dir() {
        seeds.push(cwd);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    {
        seeds.push(dir);
    }
    for seed in &seeds {
        for anc in seed.ancestors().take(5) {
            if anc.join("assets").join("puzzles.json").is_file() || anc.join("Cargo.toml").is_file() {
                return anc.to_path_buf();
            }
        }
    }
    seeds.pop().unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// 出厂内置题库路径：项目根下 `assets/puzzles.json`
/// （开发期由 ETL 预拉筛选，见 design-doc/04-题源与ETL.md §5）。
pub fn builtin_puzzles_path() -> std::path::PathBuf {
    project_root().join("assets").join("puzzles.json")
}
