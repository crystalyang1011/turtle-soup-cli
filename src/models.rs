//! 数据结构定义：Puzzle / Session / 错误码 / invoke DTO。
//! 与 docs/1-turtle-cli/03-数据结构与持久化.md 一一对应。
// by AI.Coding

use serde::{Deserialize, Serialize};

/// 应用统一错误。invoke 层序列化为 `{ ok: false, error: {...} }`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            retryable: code.retryable(),
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new(ErrorCode::Io, e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::new(ErrorCode::ParseFailed, e.to_string())
    }
}

/// 错误码枚举，见 docs/1-turtle-cli/02-技术架构.md §4.1。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NoApiKey,
    AuthFailed,
    QuotaExceeded,
    RateLimited,
    NetworkError,
    Timeout,
    ParseFailed,
    PuzzleNotFound,
    SessionNotFound,
    Io,
    InvalidConfig,
    InvalidState,
    Other,
}

impl ErrorCode {
    /// 该错误是否值得自动重试。
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::RateLimited
                | ErrorCode::NetworkError
                | ErrorCode::Timeout
                | ErrorCode::Io
        )
    }
}

// ---------------------------------------------------------------------------
// 题目
// ---------------------------------------------------------------------------

/// 一条关键事实。`core` 标记因果链的「因」与「果」，构成胜负硬条件。
/// 见 docs/1-turtle-cli/03-数据结构与持久化.md §1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyFact {
    pub text: String,
    #[serde(default)]
    pub core: bool,
}

/// 题目。`truth` 仅 Rust 侧可见，绝不随 invoke 下发前端。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Puzzle {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub surface: String,
    pub truth: String,
    pub key_facts: Vec<KeyFact>,
    #[serde(default = "default_difficulty")]
    pub difficulty: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub created_at: i64,
}

fn default_difficulty() -> u8 {
    2
}
fn default_source() -> String {
    "builtin".to_string()
}

impl Puzzle {
    pub fn fact_count(&self) -> usize {
        self.key_facts.len()
    }

    /// 转成前端可见结构（剔除 truth）。
    pub fn to_public(&self) -> PublicPuzzle {
        PublicPuzzle {
            id: self.id.clone(),
            title: self.title.clone(),
            surface: self.surface.clone(),
            key_fact_count: self.key_facts.len(),
            difficulty: self.difficulty,
            tags: self.tags.clone(),
            source: self.source.clone(),
        }
    }

    /// 基础校验：条数 ∈ [4,6]，core ∈ [1,2]（见 04 §3 / 01 §4.2）。
    pub fn validate(&self) -> Result<(), AppError> {
        let n = self.key_facts.len();
        if !(4..=6).contains(&n) {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                format!("题目 {} 的 key_facts 条数 {} 不在 [4,6]", self.id, n),
            ));
        }
        let core = self.key_facts.iter().filter(|f| f.core).count();
        if !(1..=2).contains(&core) {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                format!("题目 {} 的 core 事实数 {} 不在 [1,2]", self.id, core),
            ));
        }
        if self.surface.trim().is_empty() || self.truth.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidConfig,
                format!("题目 {} 的汤面/汤底为空", self.id),
            ));
        }
        Ok(())
    }
}

/// 前端可见的题目（无 truth）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicPuzzle {
    pub id: String,
    pub title: String,
    pub surface: String,
    pub key_fact_count: usize,
    pub difficulty: u8,
    pub tags: Vec<String>,
    pub source: String,
}

/// puzzles.json 文件结构（顶层带 schemaVersion）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuzzleFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub puzzles: Vec<Puzzle>,
}

// ---------------------------------------------------------------------------
// 对局
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Player,
    Host,
}

/// 主持人判定结果。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Judgment {
    Yes,
    No,
    Irrelevant,
    Partial,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HintLevel {
    L1,
    L2,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Playing,
    Guessing,
    Paused,
    Won,
    Lost,
    Abandoned,
}

impl SessionStatus {
    /// 未结束、可恢复的状态（见 07 §5）。
    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            SessionStatus::Playing | SessionStatus::Guessing | SessionStatus::Paused
        )
    }

    pub fn is_finished(self) -> bool {
        matches!(
            self,
            SessionStatus::Won | SessionStatus::Lost | SessionStatus::Abandoned
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<Judgment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hit_facts: Vec<usize>,
}

/// 题库变更时的恢复兜底快照，见 03 §7。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PuzzleSnapshot {
    pub surface: String,
    pub key_fact_count: usize,
    pub difficulty: u8,
}

/// 对局。字段与 03 §2 / §4 一致（磁盘 snake_case）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub puzzle_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub puzzle_snapshot: Option<PuzzleSnapshot>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub hit_facts: Vec<usize>,
    #[serde(default)]
    pub question_count: u32,
    #[serde(default)]
    pub hints_used: f32,
    #[serde(default)]
    pub hint_levels: Vec<HintLevel>,
    #[serde(default)]
    pub guess_attempts_failed: u32,
    pub status: SessionStatus,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

impl Session {
    pub fn new(id: impl Into<String>, puzzle: &Puzzle) -> Self {
        Self {
            id: id.into(),
            schema_version: crate::SCHEMA_VERSION,
            puzzle_id: puzzle.id.clone(),
            puzzle_snapshot: Some(PuzzleSnapshot {
                surface: puzzle.surface.clone(),
                key_fact_count: puzzle.key_facts.len(),
                difficulty: puzzle.difficulty,
            }),
            messages: Vec::new(),
            hit_facts: Vec::new(),
            question_count: 0,
            hints_used: 0.0,
            hint_levels: Vec::new(),
            guess_attempts_failed: 0,
            status: SessionStatus::Playing,
            started_at: now_ts(),
            ended_at: None,
        }
    }
}

/// 当前 Unix 时间戳（秒）。
pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// invoke DTO（前端契约，camelCase，见 02 §4）
// ---------------------------------------------------------------------------

/// `ask_host` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskHostResult {
    pub judgment: Judgment,
    pub reply: String,
    pub hit_facts: Vec<usize>,
    pub progress: Progress,
}

/// `judge_guess` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeGuessResult {
    pub verdict: JudgeVerdict,
    pub hit_count: usize,
    pub missed_count: usize,
    pub comment: String,
    pub score: Option<ScoreResult>,
    /// 仅在结算（赢或猜底次数耗尽）时下发汤底，供前端结算展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JudgeVerdict {
    Win,
    Lose,
}

/// 进度：命中数 / 总数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub hit: usize,
    pub total: usize,
}

/// 单局得分与星级，见 01 §3。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreResult {
    pub score: u32,
    pub stars: u8,
}

/// `use_hint` 返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintResult {
    pub text: String,
    /// L2 提示直接命中的事实序号（L1 为空）。
    pub hit_fact: Option<usize>,
}

/// 会话摘要（列表用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub puzzle_id: String,
    pub status: SessionStatus,
    pub question_count: u32,
    pub started_at: i64,
    pub surface: String,
}
