//! 对局状态机 + 持久化（原子写入 / schema 迁移 / 题库与战绩存储）。
//! 规则见 design-doc/03-数据结构与持久化.md、01-游戏设计.md §3–§5。
//! 允许依赖文件系统与 serde；不依赖 Tauri / 网络。
// by AI.Coding

use crate::engine;
use crate::models::{
    AppError, ErrorCode, HintLevel, JudgeGuessResult, JudgeVerdict, Judgment, Message, Progress,
    Puzzle, PuzzleFile, PublicPuzzle, Role, ScoreResult, Session, SessionStatus, SessionSummary,
};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 原子写入
// ---------------------------------------------------------------------------

/// JSON 原子写入：写临时文件 → rename 覆盖。见 03 §5。
pub fn write_atomic_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), AppError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut tmp, value)
        .map_err(|e| AppError::new(ErrorCode::Io, format!("序列化失败: {e}")))?;
    use std::io::Write;
    tmp.flush()?;
    // tempfile 在 Windows 上使用 MoveFileEx(REPLACE_EXISTING)，可原子覆盖。
    tmp.persist(path)
        .map_err(|e| AppError::new(ErrorCode::Io, format!("落盘失败: {}", e.error)))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

pub fn sessions_dir() -> Result<PathBuf, AppError> {
    let d = crate::app_data_dir()?.join("sessions");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// 清洗 id，避免路径穿越（仅保留字母数字与 `-_`）。
fn safe_name(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

pub fn session_path(id: &str) -> Result<PathBuf, AppError> {
    Ok(sessions_dir()?.join(format!("{}.json", safe_name(id))))
}

pub fn user_puzzles_path() -> Result<PathBuf, AppError> {
    Ok(crate::app_data_dir()?.join("puzzles.json"))
}

pub fn stats_path() -> Result<PathBuf, AppError> {
    Ok(crate::app_data_dir()?.join("stats.json"))
}

// ---------------------------------------------------------------------------
// Session 状态转移
// ---------------------------------------------------------------------------

impl Session {
    /// 追加玩家输入（提问或猜底前的输入）。
    pub fn push_player(&mut self, text: impl Into<String>) {
        self.messages.push(Message {
            role: Role::Player,
            text: text.into(),
            judgment: None,
            hit_facts: Vec::new(),
        });
    }

    /// 应用一次主持人判定：追加 host 消息、合并命中、问数 +1。见 03 §8。
    pub fn apply_judgment(
        &mut self,
        total_facts: usize,
        judgment: Judgment,
        reply: String,
        new_hits: Vec<usize>,
    ) -> Progress {
        let hits: Vec<usize> = new_hits.into_iter().filter(|i| *i < total_facts).collect();
        self.messages.push(Message {
            role: Role::Host,
            text: reply,
            judgment: Some(judgment),
            hit_facts: hits.clone(),
        });
        self.hit_facts = engine::merge_hit_facts(&self.hit_facts, &hits);
        self.question_count += 1;
        engine::progress(total_facts, &self.hit_facts)
    }

    /// 应用一次提示（L2 直接命中事实）。见 01 §5。
    pub fn apply_hint(&mut self, level: HintLevel, text: String, hit_fact: Option<usize>) {
        self.hints_used += engine::hint_cost(level);
        self.hint_levels.push(level);
        if let Some(i) = hit_fact {
            self.hit_facts = engine::merge_hit_facts(&self.hit_facts, &[i]);
        }
        self.messages.push(Message {
            role: Role::Host,
            text: format!("【提示】{text}"),
            judgment: None,
            hit_facts: hit_fact.into_iter().collect(),
        });
    }

    /// 应用一次猜底结果，推进状态机。见 01 §4。
    pub fn apply_guess(
        &mut self,
        puzzle: &Puzzle,
        hit_facts: Vec<usize>,
        comment: String,
    ) -> JudgeGuessResult {
        let verdict = engine::judge_verdict(puzzle, &hit_facts);
        let total = puzzle.fact_count();
        let hit_count = hit_facts.iter().filter(|i| **i < total).count();
        let missed_count = total - hit_count;
        self.hit_facts = engine::merge_hit_facts(&self.hit_facts, &hit_facts);
        self.messages.push(Message {
            role: Role::Host,
            text: comment.clone(),
            judgment: None,
            hit_facts: Vec::new(),
        });

        let mut score = None;
        match verdict {
            JudgeVerdict::Win => {
                self.status = SessionStatus::Won;
                self.ended_at = Some(crate::models::now_ts());
                score = Some(engine::score(
                    puzzle.difficulty,
                    self.question_count,
                    self.hints_used,
                    self.guess_attempts_failed,
                ));
            }
            JudgeVerdict::Lose => {
                // 猜底次数不设上限：失败仅计次、继续本局（见 01 §4）。
                self.guess_attempts_failed += 1;
                self.status = SessionStatus::Playing;
            }
        }

        JudgeGuessResult {
            verdict,
            hit_count,
            missed_count,
            comment,
            score,
            truth: if self.status.is_finished() {
                Some(puzzle.truth.clone())
            } else {
                None
            },
        }
    }

    /// 挂起（落盘）。
    pub fn pause(&mut self) {
        if !self.status.is_finished() {
            self.status = SessionStatus::Paused;
        }
    }

    /// 恢复：区分是否处于猜底中。
    pub fn resume(&mut self, into_guessing: bool) {
        if self.status == SessionStatus::Paused {
            self.status = if into_guessing {
                SessionStatus::Guessing
            } else {
                SessionStatus::Playing
            };
        }
    }

    pub fn abandon(&mut self) {
        self.status = SessionStatus::Abandoned;
        self.ended_at = Some(crate::models::now_ts());
    }

    pub fn to_summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            puzzle_id: self.puzzle_id.clone(),
            status: self.status,
            question_count: self.question_count,
            started_at: self.started_at,
            surface: self
                .puzzle_snapshot
                .as_ref()
                .map(|s| s.surface.clone())
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Session 持久化
// ---------------------------------------------------------------------------

pub fn save_session(s: &Session) -> Result<(), AppError> {
    write_atomic_json(&session_path(&s.id)?, s)
}

/// 迁移链：低版本 → 当前版本。当前仅 v1，保留框架（见 03 §6）。
fn migrate_session_json(mut v: Value) -> Result<Value, AppError> {
    let ver = v
        .get("schemaVersion")
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as u32;
    if ver > crate::SCHEMA_VERSION {
        return Err(AppError::new(
            ErrorCode::InvalidState,
            format!("会话 schema 版本 {ver} 高于当前支持的 {}", crate::SCHEMA_VERSION),
        ));
    }
    // v0 → v1：补 schemaVersion（历史无版本文件视为 v0）。
    if ver < 1 {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("schemaVersion".into(), Value::from(1));
        }
    }
    // 未来：v1→v2 ...
    Ok(v)
}

pub fn load_session(id: &str) -> Result<Session, AppError> {
    let path = session_path(id)?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        AppError::new(ErrorCode::SessionNotFound, format!("会话 {id} 不存在"))
    })?;
    let v: Value = serde_json::from_str(&text)?;
    let v = migrate_session_json(v)?;
    serde_json::from_value(v).map_err(|e| {
        AppError::new(ErrorCode::ParseFailed, format!("会话 {id} 解析失败: {e}"))
    })
}

/// 列出所有会话摘要；坏文件跳过（改名 .corrupt）。见 07 §5。
pub fn list_sessions() -> Result<Vec<SessionSummary>, AppError> {
    let dir = sessions_dir()?;
    let mut out = Vec::new();
    for ent in std::fs::read_dir(&dir)? {
        let ent = ent?;
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = (|| -> Result<Session, AppError> {
            let text = std::fs::read_to_string(&path)?;
            let v: Value = serde_json::from_str(&text)?;
            let v = migrate_session_json(v)?;
            serde_json::from_value(v)
                .map_err(|e| AppError::new(ErrorCode::ParseFailed, e.to_string()))
        })();
        match parsed {
            Ok(s) => out.push(s.to_summary()),
            Err(_) => {
                let mut bad = path.clone();
                bad.set_extension("corrupt");
                let _ = std::fs::rename(&path, &bad);
            }
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
    Ok(out)
}

/// 恢复中的会话（未结束）。
pub fn list_resumable() -> Result<Vec<SessionSummary>, AppError> {
    Ok(list_sessions()?
        .into_iter()
        .filter(|s| s.status.is_resumable())
        .collect())
}

pub fn delete_session(id: &str) -> Result<(), AppError> {
    let path = session_path(id)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 题库存储（内置 + 用户拉取），见 03 §4、04 §5
// ---------------------------------------------------------------------------

/// 题目仓库：内置题来自 `assets/puzzles.json`，用户题来自 app data `puzzles.json`。
#[derive(Clone)]
pub struct PuzzleStore {
    builtin: Vec<Puzzle>,
    user: Vec<Puzzle>,
}

impl PuzzleStore {
    /// 加载题库：内置题（出厂 `assets/puzzles.json`）+ 用户题（app data）。
    ///
    /// 文件**存在但读取/解析失败**时返回明确错误，**不静默降级为空题库**
    /// （见 02 §4.3、07 §4）；文件缺失才视为空（提示用户 `fetch`）。
    pub fn load() -> Result<Self, AppError> {
        let builtin_path = crate::builtin_puzzles_path();
        let builtin = if builtin_path.is_file() {
            load_puzzle_file(&builtin_path)?
        } else {
            crate::log_warn!(
                "session",
                "内置题库缺失：{}（可执行 `soup-cli fetch` 拉题）",
                builtin_path.display()
            );
            Vec::new()
        };
        let user = match user_puzzles_path() {
            Ok(p) if p.is_file() => load_puzzle_file(&p)?,
            _ => Vec::new(),
        };
        Ok(Self { builtin, user })
    }

    /// 全部题目（用户题在后）。
    pub fn all(&self) -> Vec<&Puzzle> {
        self.builtin.iter().chain(self.user.iter()).collect()
    }

    pub fn len(&self) -> usize {
        self.builtin.len() + self.user.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &str) -> Option<&Puzzle> {
        self.user
            .iter()
            .find(|p| p.id == id)
            .or_else(|| self.builtin.iter().find(|p| p.id == id))
    }

    /// 用户可见列表（无 truth）。
    pub fn public_list(&self) -> Vec<PublicPuzzle> {
        self.all().iter().map(|p| p.to_public()).collect()
    }

    /// 按难度与结果筛选。`difficulty=None` 表示不限。
    pub fn candidates(&self, difficulty: Option<u8>) -> Vec<&Puzzle> {
        let mut v: Vec<&Puzzle> = self
            .all()
            .into_iter()
            .filter(|p| difficulty.map_or(true, |d| p.difficulty == d))
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// 新增用户题（按 id 去重，返回实际新增数）。校验不通过的不入库（见 04 §3）。
    pub fn add_user_puzzles(&mut self, puzzles: Vec<Puzzle>) -> usize {
        let mut added = 0;
        for p in puzzles {
            if p.validate().is_err() {
                continue;
            }
            let dup = self.builtin.iter().any(|x| x.id == p.id)
                || self.user.iter().any(|x| x.id == p.id);
            if !dup {
                self.user.push(p);
                added += 1;
            }
        }
        added
    }

    /// 删除用户题（内置题不可删）。
    pub fn delete_user_puzzle(&mut self, id: &str) -> bool {
        let before = self.user.len();
        self.user.retain(|p| p.id != id);
        before != self.user.len()
    }

    pub fn save_user(&self) -> Result<(), AppError> {
        let file = PuzzleFile {
            schema_version: crate::SCHEMA_VERSION,
            puzzles: self.user.clone(),
        };
        write_atomic_json(&user_puzzles_path()?, &file)
    }
}

fn load_puzzle_file(path: &Path) -> Result<Vec<Puzzle>, AppError> {
    let text = std::fs::read_to_string(path)?;
    let file: PuzzleFile = serde_json::from_str(&text)?;
    Ok(file.puzzles)
}

// ---------------------------------------------------------------------------
// 战绩记录，见 01 §9
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StatsFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub records: Vec<StatsRecord>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StatsRecord {
    pub session_id: String,
    pub puzzle_id: String,
    pub difficulty: u8,
    pub question_count: u32,
    pub hints_used: f32,
    pub guess_attempts_failed: u32,
    pub status: SessionStatus,
    pub score: Option<ScoreResult>,
    pub ended_at: i64,
}

/// 追加一条战绩。
pub fn record_stats(rec: StatsRecord) -> Result<(), AppError> {
    let path = stats_path()?;
    let mut file = if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        serde_json::from_str::<StatsFile>(&text).unwrap_or(StatsFile {
            schema_version: crate::SCHEMA_VERSION,
            records: Vec::new(),
        })
    } else {
        StatsFile {
            schema_version: crate::SCHEMA_VERSION,
            records: Vec::new(),
        }
    };
    file.records.push(rec);
    write_atomic_json(&path, &file)
}

// ---------------------------------------------------------------------------
// 隐藏题目（"不再显示"），见 03 §4
// ---------------------------------------------------------------------------

/// 隐藏题目清单（app data `hidden.json`）。独立于题库存储：
/// 内置题库只读，隐藏状态单独记，便于随时 `/unhide` 恢复。
#[derive(Debug, Clone, Serialize, serde::Deserialize, Default)]
pub struct HiddenFile {
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: u32,
    #[serde(default)]
    pub hidden_ids: Vec<String>,
}

pub fn hidden_path() -> Result<PathBuf, AppError> {
    Ok(crate::app_data_dir()?.join("hidden.json"))
}

/// 读取隐藏集合。文件缺失或损坏按空处理（不影响开局，见 07 §4）。
pub fn load_hidden() -> std::collections::HashSet<String> {
    let Ok(p) = hidden_path() else {
        return std::collections::HashSet::new();
    };
    if !p.is_file() {
        return std::collections::HashSet::new();
    }
    match std::fs::read_to_string(&p).map_err(AppError::from).and_then(|t| {
        serde_json::from_str::<HiddenFile>(&t).map_err(AppError::from)
    }) {
        Ok(f) => f.hidden_ids.into_iter().collect(),
        Err(e) => {
            crate::log_warn!("session", "hidden.json 读取失败，按空处理: {e}");
            std::collections::HashSet::new()
        }
    }
}

fn save_hidden(ids: &std::collections::HashSet<String>) -> Result<(), AppError> {
    let mut list: Vec<String> = ids.iter().cloned().collect();
    list.sort();
    let file = HiddenFile { schema_version: crate::SCHEMA_VERSION, hidden_ids: list };
    write_atomic_json(&hidden_path()?, &file)
}

/// 标记题目"不再显示"（幂等）。
pub fn hide_puzzle(id: &str) -> Result<(), AppError> {
    let mut ids = load_hidden();
    ids.insert(id.to_string());
    save_hidden(&ids)
}

/// 取消隐藏；返回是否确实从隐藏集合中移除。
pub fn unhide_puzzle(id: &str) -> Result<bool, AppError> {
    let mut ids = load_hidden();
    let removed = ids.remove(id);
    if removed {
        save_hidden(&ids)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::KeyFact;

    fn puzzle() -> Puzzle {
        Puzzle {
            id: "classic-t".into(),
            title: "t".into(),
            surface: "s".into(),
            truth: "truth".into(),
            key_facts: vec![
                KeyFact { text: "a".into(), core: true },
                KeyFact { text: "b".into(), core: false },
                KeyFact { text: "c".into(), core: false },
                KeyFact { text: "d".into(), core: true },
            ],
            difficulty: 2,
            tags: vec![],
            source: "builtin".into(),
            created_at: 0,
        }
    }

    #[test]
    fn apply_judgment_merges_and_counts() {
        let p = puzzle();
        let mut s = Session::new("s1", &p);
        s.push_player("他在打嗝吗");
        let prog = s.apply_judgment(4, Judgment::Yes, "是".into(), vec![0, 0]);
        assert_eq!(prog.hit, 1);
        assert_eq!(prog.total, 4);
        assert_eq!(s.question_count, 1);
        assert_eq!(s.messages.len(), 2);
    }

    #[test]
    fn guess_win_sets_status_and_score() {
        let p = puzzle();
        let mut s = Session::new("s2", &p);
        s.question_count = 8;
        let r = s.apply_guess(&p, vec![0, 1, 2, 3], "不错".into());
        assert_eq!(r.verdict, JudgeVerdict::Win);
        assert_eq!(s.status, SessionStatus::Won);
        assert!(r.score.is_some());
        assert!(s.ended_at.is_some());
    }

    #[test]
    fn guess_fail_keeps_playing_unlimited() {
        let p = puzzle();
        let mut s = Session::new("s3", &p);
        let r1 = s.apply_guess(&p, vec![0], "差".into());
        assert_eq!(r1.verdict, JudgeVerdict::Lose);
        assert_eq!(s.status, SessionStatus::Playing);
        assert_eq!(s.guess_attempts_failed, 1);
        // 不限次数：连续失败也不会进 lost。
        s.apply_guess(&p, vec![0], "差".into());
        s.apply_guess(&p, vec![0], "差".into());
        s.apply_guess(&p, vec![0], "差".into());
        assert_eq!(s.status, SessionStatus::Playing);
        assert_eq!(s.guess_attempts_failed, 4);
        assert!(s.ended_at.is_none());
    }

    #[test]
    fn hint_updates_cost_and_hits() {
        let p = puzzle();
        let mut s = Session::new("s4", &p);
        s.apply_hint(HintLevel::L1, "方向".into(), None);
        assert_eq!(s.hints_used, 0.5);
        s.apply_hint(HintLevel::L2, "事实c".into(), Some(2));
        assert_eq!(s.hints_used, 1.5);
        assert!(s.hit_facts.contains(&2));
    }

    #[test]
    fn pause_and_resume() {
        let p = puzzle();
        let mut s = Session::new("s5", &p);
        s.pause();
        assert_eq!(s.status, SessionStatus::Paused);
        s.resume(true);
        assert_eq!(s.status, SessionStatus::Guessing);
    }

    #[test]
    fn atomic_write_roundtrip_and_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("x.json");
        write_atomic_json(&path, &puzzle()).unwrap();
        write_atomic_json(&path, &puzzle()).unwrap(); // 覆盖不报错
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"id\""));
    }

    #[test]
    fn migrate_v0_adds_schema_version() {
        let raw = serde_json::json!({ "id": "s", "puzzle_id": "p", "status": "playing",
            "started_at": 0, "messages": [], "hit_facts": [], "question_count": 0,
            "hints_used": 0.0, "hint_levels": [], "guess_attempts_failed": 0 });
        let v = migrate_session_json(raw).unwrap();
        assert_eq!(v.get("schemaVersion").and_then(|x| x.as_u64()), Some(1));
    }

    #[test]
    fn migrate_rejects_future_version() {
        let raw = serde_json::json!({ "schemaVersion": 999 });
        assert!(migrate_session_json(raw).is_err());
    }

    #[test]
    fn store_add_dedup_and_delete() {
        let mut store = PuzzleStore { builtin: vec![puzzle()], user: vec![] };
        let p2 = Puzzle { id: "ai-1".into(), ..puzzle() };
        assert_eq!(store.add_user_puzzles(vec![p2.clone(), p2.clone()]), 1);
        // 与内置同 id 不入库
        let dup = Puzzle { id: "classic-t".into(), ..puzzle() };
        assert_eq!(store.add_user_puzzles(vec![dup]), 0);
        assert!(store.delete_user_puzzle("ai-1"));
        assert!(!store.delete_user_puzzle("classic-t")); // 内置不可删
    }

    #[test]
    fn store_rejects_invalid_puzzle() {
        let mut store = PuzzleStore { builtin: vec![], user: vec![] };
        let bad = Puzzle {
            id: "bad".into(),
            key_facts: vec![KeyFact { text: "only".into(), core: true }],
            ..puzzle()
        };
        assert_eq!(store.add_user_puzzles(vec![bad]), 0);
    }
}
