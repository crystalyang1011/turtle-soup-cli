//! 数据集直链下载 + 本地 ETL + 断点游标。
//! 策略见 design-doc/04-题源与ETL.md §3、§5、§7、§8。
//!
//! 数据形态（2026-10 已核验）：中文集为单个 JSONL，**每行一条"猜测-标注对"**
//! （`id/title/surface/bottom/user_guess/label`）。ETL 先按 `surface`（辅以 `bottom`）
//! 分组，再收集组内 `label=="T"` 的 `user_guess` 作为 key_facts 候选。
// by AI.Coding

use crate::models::{AppError, ErrorCode, KeyFact, Puzzle};
use crate::session::{write_atomic_json, PuzzleStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// key_facts 条数下限（与 scripts/etl_rules.json 保持一致，见 04 §3）。
pub const MIN_FACTS: usize = 4;
/// key_facts 条数上限；超出按原序机械截断（与 scripts/etl_rules.json 保持一致，见 04 §3）。
pub const MAX_FACTS: usize = 6;

/// 官方直链（中文集 JSONL，见 04 §5.5）。
pub const DEFAULT_ENDPOINT: &str =
    "https://huggingface.co/datasets/Duguce/TurtleBench1.5k/resolve/main/chinese/zh_data-00000-of-00001.jsonl";
/// 国内镜像直链（`--mirror`，见 04 §5.5）。
pub const MIRROR_ENDPOINT: &str =
    "https://hf-mirror.com/datasets/Duguce/TurtleBench1.5k/resolve/main/chinese/zh_data-00000-of-00001.jsonl";

/// ETL 过滤词（**只过滤血腥/色情，不过滤恐怖/灵异**，见 04 §4）。
pub const BLOCKED_KEYWORDS: &[&str] = &[
    "血腥", "血泊", "肢解", "断肢", "残肢", "内脏", "割喉", "自残", "奸杀", "碎尸",
    "色情", "裸体", "性交", "强奸", "淫", "肉欲",
];

// ---------------------------------------------------------------------------
// 断点游标
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FetchCursor {
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: u32,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub consumed_ids: Vec<String>,
}

impl FetchCursor {
    pub fn path() -> Result<PathBuf, AppError> {
        Ok(crate::data_dir()?.join("fetch_cursor.json"))
    }

    pub fn load() -> Result<Self, AppError> {
        let p = Self::path()?;
        if !p.exists() {
            return Ok(Self { schema_version: crate::SCHEMA_VERSION, ..Default::default() });
        }
        let text = std::fs::read_to_string(&p)?;
        let mut c: FetchCursor = serde_json::from_str(&text)?;
        if c.schema_version == 0 {
            c.schema_version = crate::SCHEMA_VERSION;
        }
        Ok(c)
    }

    pub fn save(&self) -> Result<(), AppError> {
        write_atomic_json(&Self::path()?, self)
    }

    pub fn is_consumed(&self, id: &str) -> bool {
        self.consumed_ids.iter().any(|x| x == id)
    }

    /// 记录一批消费（幂等去重）。
    pub fn mark_consumed(&mut self, ids: &[String]) {
        for id in ids {
            if !self.is_consumed(id) {
                self.consumed_ids.push(id.clone());
            }
        }
        self.offset += ids.len();
    }
}

// ---------------------------------------------------------------------------
// 原始记录与分组
// ---------------------------------------------------------------------------

/// JSONL 单行（一条"猜测-标注对"）。字段名兼容旧 rows API 包裹与别名。
#[derive(Debug, Clone)]
pub struct RawGuess {
    pub title: String,
    pub surface: String,
    pub bottom: String,
    pub user_guess: String,
    pub label: String,
}

impl RawGuess {
    /// 从一行 JSON（已解析）宽容解析；缺 surface/bottom 视为无效行。
    pub fn from_value(v: &Value) -> Option<Self> {
        let row = v.get("row").unwrap_or(v);
        let surface = str_field(row, &["surface", "story", "puzzle", "soup_surface"])?;
        let bottom = str_field(row, &["bottom", "truth", "answer", "soup_bottom"])?;
        let title = str_field(row, &["title", "name"]).unwrap_or_default();
        let user_guess =
            str_field(row, &["user_guess", "guess", "question"]).unwrap_or_default();
        let label = str_field(row, &["label", "verdict"]).unwrap_or_default();
        Some(Self { title, surface, bottom, user_guess, label })
    }

    /// 是否人工标注为"正确"（T）。见 04 §3。
    pub fn is_correct(&self) -> bool {
        matches!(
            self.label.trim().to_ascii_uppercase().as_str(),
            "T" | "TRUE" | "YES" | "正确"
        )
    }
}

/// 一条故事：汤面/汤底 + 组内 T 标注猜测（key_facts 候选，见 04 §3、§8）。
#[derive(Debug, Clone)]
pub struct RawStory {
    pub title: String,
    pub surface: String,
    pub bottom: String,
    pub positive_guesses: Vec<String>,
}

impl RawStory {
    /// 故事级稳定 id：`ds-<surface 稳定哈希>`（见 04 §8）。
    pub fn id(&self) -> String {
        format!("ds-{}", short_hash(&self.surface))
    }
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        v.get(*k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
    })
}

/// 解析 JSONL 文本为逐行猜测记录。返回 (记录, 非法行数)。
pub fn parse_jsonl(text: &str) -> (Vec<RawGuess>, usize) {
    let mut out = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => match RawGuess::from_value(&v) {
                Some(g) => out.push(g),
                None => bad += 1,
            },
            Err(_) => bad += 1,
        }
    }
    (out, bad)
}

/// 把逐行猜测按故事分组（`surface` + `bottom` 为主键），收集 `label=="T"` 的猜测。
/// 组内去重、保持原序（见 04 §3、§8）。
pub fn group_stories(guesses: &[RawGuess]) -> Vec<RawStory> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<RawStory> = Vec::new();
    for g in guesses {
        let key = format!("{}\u{1}{}", g.surface, g.bottom);
        let idx = match index.get(&key) {
            Some(i) => *i,
            None => {
                let i = out.len();
                index.insert(key, i);
                out.push(RawStory {
                    title: g.title.clone(),
                    surface: g.surface.clone(),
                    bottom: g.bottom.clone(),
                    positive_guesses: Vec::new(),
                });
                i
            }
        };
        let guess = g.user_guess.trim();
        if g.is_correct()
            && !guess.is_empty()
            && !out[idx].positive_guesses.iter().any(|x| x == guess)
        {
            out[idx].positive_guesses.push(guess.to_string());
        }
    }
    out
}

fn short_hash(s: &str) -> String {
    // 简易稳定哈希，仅用于生成稳定 id。
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

/// 内容过滤：命中血腥/色情词返回 true（**不含恐怖/灵异**，见 04 §4）。
pub fn is_blocked(text: &str) -> bool {
    BLOCKED_KEYWORDS.iter().any(|k| text.contains(k))
}

/// ETL：RawStory → Puzzle。key_facts 取 T 标注去重后的原序，不足 4 条丢弃，
/// 超过 6 条机械截断（首条 core=true）；语义合并改写由人工/AI 后处理负责（见 04 §3、§7）。
pub fn etl_to_puzzle(raw: &RawStory, difficulty: u8) -> Option<Puzzle> {
    if is_blocked(&raw.surface) || is_blocked(&raw.bottom) {
        return None;
    }
    let mut facts: Vec<String> = Vec::new();
    for g in &raw.positive_guesses {
        let g = g.trim();
        if g.is_empty() || facts.iter().any(|f| f == g) {
            continue;
        }
        facts.push(g.to_string());
    }
    if facts.len() < MIN_FACTS {
        return None;
    }
    facts.truncate(MAX_FACTS);
    let key_facts = facts
        .into_iter()
        .enumerate()
        .map(|(i, text)| KeyFact { text, core: i == 0 })
        .collect();
    let title = if raw.title.trim().is_empty() {
        raw.surface.chars().take(12).collect()
    } else {
        raw.title.trim().to_string()
    };
    let puzzle = Puzzle {
        id: raw.id(),
        title,
        surface: raw.surface.clone(),
        truth: raw.bottom.clone(),
        key_facts,
        difficulty: difficulty.clamp(1, 5),
        tags: vec!["dataset".into()],
        source: "dataset".into(),
        created_at: crate::models::now_ts(),
    };
    puzzle.validate().ok()?;
    Some(puzzle)
}

/// 原始记录落盘缓存（见 04 §5.4）。
pub fn cache_raw(batch_index: usize, rows: &[Value]) -> Result<PathBuf, AppError> {
    let dir = crate::data_dir()?.join("raw");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("batch-{batch_index:05}.json"));
    write_atomic_json(&path, rows)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 拉取
// ---------------------------------------------------------------------------

/// 数据集客户端（直链下载 JSONL）。
#[derive(Clone)]
pub struct DatasetClient {
    http: reqwest::Client,
    endpoint: String,
}

impl DatasetClient {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AppError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AppError::new(ErrorCode::Other, format!("HTTP 初始化失败: {e}")))?;
        Ok(Self { http, endpoint: endpoint.into() })
    }

    /// 直链下载完整 JSONL 文本（约 1 MB）。
    pub async fn fetch_all(&self) -> Result<String, AppError> {
        let resp = self
            .http
            .get(&self.endpoint)
            .send()
            .await
            .map_err(|e| AppError::new(ErrorCode::NetworkError, format!("拉取失败: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::new(
                ErrorCode::NetworkError,
                format!("拉取失败: HTTP {}", resp.status()),
            ));
        }
        resp.text()
            .await
            .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("响应读取失败: {e}")))
    }

    /// 下载 → 落盘缓存 → 分组 → ETL → 入库。返回故事总数与实际新增题数。见 04 §5。
    pub async fn pull_into(
        &self,
        store: &mut PuzzleStore,
        cursor: &mut FetchCursor,
        difficulty: u8,
    ) -> Result<PullOutcome, AppError> {
        let text = self.fetch_all().await?;

        // 逐行解析：同时保留原始 JSON（缓存用）与有效猜测记录。
        let mut rows: Vec<Value> = Vec::new();
        let mut guesses: Vec<RawGuess> = Vec::new();
        let mut bad = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(v) => {
                    if let Some(g) = RawGuess::from_value(&v) {
                        guesses.push(g);
                    } else {
                        bad += 1;
                    }
                    rows.push(v);
                }
                Err(_) => bad += 1,
            }
        }
        if rows.is_empty() {
            return Err(AppError::new(
                ErrorCode::ParseFailed,
                "远程数据集为空或格式不符（预期 JSONL）",
            ));
        }
        cache_raw(0, &rows)?;
        if bad > 0 {
            crate::log_warn!("dataset", "拉取跳过 {bad} 行非法 JSONL");
        }

        let stories = group_stories(&guesses);
        let fetched = stories.len();
        let mut puzzles = Vec::new();
        let mut consumed = Vec::new();
        for s in &stories {
            let id = s.id();
            consumed.push(id.clone());
            if cursor.is_consumed(&id) {
                continue; // 幂等：已消费跳过
            }
            if let Some(p) = etl_to_puzzle(s, difficulty) {
                puzzles.push(p);
            }
        }
        let added = store.add_user_puzzles(puzzles);
        store.save_user()?;
        cursor.mark_consumed(&consumed);
        cursor.save()?;

        Ok(PullOutcome { fetched, added })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PullOutcome {
    /// 解析出的唯一故事数。
    pub fetched: usize,
    /// 实际新增入库题数。
    pub added: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(surface: &str, bottom: &str, guesses: usize) -> RawStory {
        RawStory {
            title: String::new(),
            surface: surface.into(),
            bottom: bottom.into(),
            positive_guesses: (0..guesses).map(|i| format!("fact {i}")).collect(),
        }
    }

    #[test]
    fn filter_blocks_gore_and_nsfw_not_horror() {
        assert!(is_blocked("现场非常血腥"));
        assert!(is_blocked("含有色情描写"));
        // 恐怖/灵异不过滤（04 §4）
        assert!(!is_blocked("深夜的鬼屋传来诡异哭声"));
        assert!(!is_blocked("一个男人在酒吧要水"));
    }

    #[test]
    fn etl_requires_at_least_four_facts() {
        assert!(etl_to_puzzle(&raw("s", "t", 3), 2).is_none());
        assert!(etl_to_puzzle(&raw("s", "t", 4), 2).is_some());
    }

    #[test]
    fn etl_truncates_over_six_facts() {
        let p = etl_to_puzzle(&raw("s", "t", 9), 2).unwrap();
        assert_eq!(p.key_facts.len(), MAX_FACTS);
        assert_eq!(p.key_facts.iter().filter(|f| f.core).count(), 1);
    }

    #[test]
    fn etl_dedups_and_marks_core() {
        let mut r = raw("s", "t", 5);
        r.positive_guesses[1] = r.positive_guesses[0].clone(); // 5 条中 1 条重复 → 去重后 4 条
        let p = etl_to_puzzle(&r, 3).unwrap();
        assert_eq!(p.key_facts.len(), 4);
        assert_eq!(p.key_facts.iter().filter(|f| f.core).count(), 1);
        assert_eq!(p.source, "dataset");
    }

    #[test]
    fn etl_drops_blocked_content() {
        assert!(etl_to_puzzle(&raw("血腥的现场", "t", 5), 2).is_none());
    }

    #[test]
    fn cursor_is_idempotent() {
        let mut c = FetchCursor::default();
        c.mark_consumed(&["a".into(), "b".into()]);
        c.mark_consumed(&["b".into(), "c".into()]);
        assert_eq!(c.consumed_ids, vec!["a", "b", "c"]);
        assert_eq!(c.offset, 4);
        assert!(c.is_consumed("c"));
    }

    #[test]
    fn parse_jsonl_groups_by_story_and_keeps_only_t() {
        let text = concat!(
            r#"{"id":0,"title":"电梯","surface":"S1","bottom":"B1","user_guess":"g1","label":"T"}"#,
            "\n",
            r#"{"id":1,"title":"电梯","surface":"S1","bottom":"B1","user_guess":"g2","label":"F"}"#,
            "\n",
            r#"{"id":2,"title":"电梯","surface":"S1","bottom":"B1","user_guess":"g1","label":"T"}"#,
            "\n",
            r#"{"id":3,"title":"山顶","surface":"S2","bottom":"B2","user_guess":"g3","label":"T"}"#,
            "\n",
            "not-json",
        );
        let (guesses, bad) = parse_jsonl(text);
        assert_eq!(guesses.len(), 4);
        assert_eq!(bad, 1);
        let stories = group_stories(&guesses);
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].surface, "S1");
        assert_eq!(stories[0].positive_guesses, vec!["g1"]); // F 剔除、重复去重
        assert_eq!(stories[1].positive_guesses, vec!["g3"]);
    }

    #[test]
    fn raw_guess_requires_surface_and_bottom() {
        assert!(RawGuess::from_value(&serde_json::json!({ "x": 1 })).is_none());
        assert!(RawGuess::from_value(
            &serde_json::json!({ "surface": "s", "bottom": "b", "user_guess": "u", "label": "T" })
        )
        .is_some());
    }

    #[test]
    fn story_id_is_stable_and_prefixed() {
        let a = raw("同一个汤面", "t", 4);
        let b = raw("同一个汤面", "t", 4);
        assert_eq!(a.id(), b.id());
        assert!(a.id().starts_with("ds-"));
    }

    /// 防 drift：Rust 常量必须与 scripts/etl_rules.json 一致（见 04 §7）。
    #[test]
    fn shared_etl_rules_match_rust_constants() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts").join("etl_rules.json");
        let text = std::fs::read_to_string(path).expect("etl_rules.json 缺失");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["minFacts"].as_u64().unwrap() as usize, MIN_FACTS);
        assert_eq!(v["maxFacts"].as_u64().unwrap() as usize, MAX_FACTS);
        let kw: Vec<String> = v["blockedKeywords"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        let expected: Vec<String> = BLOCKED_KEYWORDS.iter().map(|s| s.to_string()).collect();
        assert_eq!(kw, expected);
    }
}
