//! 海龟汤摸鱼游戏 · CLI 版核心库。
//!
//! 纯 Rust，无 GUI：`engine` / `session` / `llm` / `dataset` / `game` 全部可测，
//! 由 `soup-cli` 二进制驱动（见 docs/1-turtle-cli/02-技术架构.md）。
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
pub mod ui;

pub use models::AppError;
pub use models::ErrorCode;

/// 当前 schema 版本（puzzles.json / sessions/*.json / stats.json）。
pub const SCHEMA_VERSION: u32 = 1;

/// 旧版系统 app data 目录名（仅用于一次性迁移，见 `migrate_legacy_data_dir`）。
pub const APP_DIR_NAME: &str = "turtle-soup";

/// 运行时数据目录名（**项目根**下，见 docs/1-turtle-cli/03 §4）。
pub const DATA_DIR_NAME: &str = "data";

/// 返回运行时数据目录：**项目根下的 `data/`**（见 docs/1-turtle-cli/03 §4）。
///
/// 数据随仓库/二进制落地，便携、可被 `.gitignore` 排除，不再写入系统 app data。
/// 首次运行时若检测到旧版系统 app data 目录，则整体迁移过来（保留题库/配置/对局）。
pub fn data_dir() -> Result<std::path::PathBuf, AppError> {
    let dir = project_root().join(DATA_DIR_NAME);
    if !dir.exists() {
        migrate_legacy_data_dir(&dir);
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 旧版数据目录（系统 app data），仅用于一次性迁移。
fn legacy_data_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", APP_DIR_NAME).map(|d| d.data_dir().to_path_buf())
}

/// 首次运行时把旧版系统 app data 目录整体搬到项目内，尽量不丢用户数据。
///
/// 这里**不使用 logging 宏**：日志路径本身依赖 `data_dir`，会递归。
fn migrate_legacy_data_dir(new_dir: &std::path::Path) {
    let Some(old) = legacy_data_dir() else { return };
    if !old.is_dir() || old == new_dir {
        return;
    }
    match std::fs::rename(&old, new_dir) {
        Ok(()) => eprintln!("[信息] 已迁移数据目录：{} -> {}", old.display(), new_dir.display()),
        Err(e) => eprintln!("[警告] 旧数据目录迁移失败（忽略）：{e}"),
    }
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
/// （开发期由 ETL 预拉筛选，见 docs/1-turtle-cli/04-题源与ETL.md §5）。
pub fn builtin_puzzles_path() -> std::path::PathBuf {
    project_root().join("assets").join("puzzles.json")
}
