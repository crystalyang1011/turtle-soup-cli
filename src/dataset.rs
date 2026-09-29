//! 数据集分批拉取 + 本地 ETL + 断点游标。
//! 策略见 design-doc/04-题源与ETL.md §5、§7、§8。
//!
//! 说明：数据集真实字段格式尚未核验（04 §6.3），此处按「预期字段映射」实现，
//! 解析层对字段缺失保持宽容，核验真实数据后只需调整 `RawStory::from_value`。
// by AI.Coding

use crate::models::{AppError, ErrorCode, KeyFact, Puzzle};
use crate::session::{write_atomic_json, PuzzleStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// 每批拉取条数（见 04 §5）。
pub const BATCH_SIZE: usize = 100;
/// 默认端点（HuggingFace datasets-server rows API）。
pub const DEFAULT_ENDPOINT: &str =
    "https://datasets-server.huggingface.co/rows?dataset=Duguce%2FTurtleBench1.5k&config=default&split=train";
/// 国内镜像提示（见 04 §5.5）。
pub const MIRROR_ENDPOINT: &str = "https://hf-mirror.com";

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
        Ok(crate::app_data_dir()?.join("fetch_cursor.json"))
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
// 原始记录与映射
// ---------------------------------------------------------------------------

/// 数据集原始故事记录（staging 预期结构）。
#[derive(Debug, Clone)]
pub struct RawStory {
    pub id: String,
    pub surface: String,
    pub bottom: String,
    /// 人工标注为「正确（T）」的玩家猜测，用于挖掘 key_facts（见 04 §3）。
    pub positive_guesses: Vec<String>,
}

impl RawStory {
    /// 从 datasets-server 的 row JSON 宽容解析。
    /// 兼容 `row` 包裹与扁平结构、`surface`/`bottom` 字段名。
    pub fn from_value(v: &Value) -> Option<Self> {
        let row = v.get("row").unwrap_or(v);
        let surface = str_field(row, &["surface", "story", "puzzle", "soup_surface"])?;
        let bottom = str_field(row, &["bottom", "truth", "answer", "soup_bottom"])?;
        let id = str_field(row, &["id", "sid", "story_id"])
            .unwrap_or_else(|| format!("ds-{}", short_hash(&surface)));
        let positive_guesses = collect_positive(row);
        Some(Self { id, surface, bottom, positive_guesses })
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

/// 收集标注为正确的猜测（字段名待核验，容忍多种形态）。
fn collect_positive(row: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["positive_guesses", "truths", "t_guesses", "correct_guesses"] {
        if let Some(arr) = row.get(key).and_then(|x| x.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    out.push(s.trim().to_string());
                }
            }
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

/// ETL：raw → Puzzle。key_facts 由正例猜测去重合并而成（此处仅做机械去重，
/// 语义合并改写由开发期脚本 + 人工负责，见 04 §3、§7）。
/// 不足 4 条不入库；过滤命中直接丢弃。
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
    if !(4..=6).contains(&facts.len()) {
        return None;
    }
    let key_facts = facts
        .into_iter()
        .enumerate()
        .map(|(i, text)| KeyFact { text, core: i == 0 })
        .collect();
    let puzzle = Puzzle {
        id: format!("ds-{}", raw.id),
        title: raw.surface.chars().take(12).collect(),
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
    let dir = crate::app_data_dir()?.join("raw");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("batch-{batch_index:05}.json"));
    write_atomic_json(&path, rows)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 拉取
// ---------------------------------------------------------------------------

/// 数据集客户端。
#[derive(Clone)]
pub struct DatasetClient {
    http: reqwest::Client,
    endpoint: String,
}

impl DatasetClient {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AppError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::new(ErrorCode::Other, format!("HTTP 初始化失败: {e}")))?;
        Ok(Self { http, endpoint: endpoint.into() })
    }

    /// 拉取一批（offset 起始，length=BATCH_SIZE）。返回原始 row 数组。
    pub async fn fetch_batch(&self, offset: usize) -> Result<Vec<Value>, AppError> {
        let url = format!("{}&offset={}&length={}", self.endpoint, offset, BATCH_SIZE);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::new(ErrorCode::NetworkError, format!("拉取失败: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::new(
                ErrorCode::NetworkError,
                format!("拉取失败: HTTP {}", resp.status()),
            ));
        }
        let json: Value = resp
            .json()
            .await
            .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("拉取解析失败: {e}")))?;
        Ok(json
            .get("rows")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// 拉取一批 → 落盘缓存 → ETL → 入库。返回实际新增题数与原始条数。见 04 §5.2。
    pub async fn pull_into(
        &self,
        store: &mut PuzzleStore,
        cursor: &mut FetchCursor,
        difficulty: u8,
    ) -> Result<PullOutcome, AppError> {
        let rows = self.fetch_batch(cursor.offset).await?;
        if rows.is_empty() {
            return Ok(PullOutcome { fetched: 0, added: 0, done: true });
        }
        cache_raw(cursor.offset / BATCH_SIZE.max(1), &rows)?;

        let mut puzzles = Vec::new();
        let mut consumed = Vec::new();
        for row in &rows {
            if let Some(raw) = RawStory::from_value(row) {
                consumed.push(raw.id.clone());
                if cursor.is_consumed(&raw.id) {
                    continue; // 幂等：已消费跳过
                }
                if let Some(p) = etl_to_puzzle(&raw, difficulty) {
                    puzzles.push(p);
                }
            }
        }
        let added = store.add_user_puzzles(puzzles);
        store.save_user()?;
        cursor.mark_consumed(&consumed);
        cursor.save()?;

        Ok(PullOutcome { fetched: rows.len(), added, done: rows.len() < BATCH_SIZE })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PullOutcome {
    pub fetched: usize,
    pub added: usize,
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(surface: &str, bottom: &str, guesses: usize) -> RawStory {
        RawStory {
            id: "r1".into(),
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
        assert!(etl_to_puzzle(&raw("s", "t", 7), 2).is_none()); // >6 也放弃
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
    fn parse_from_dataset_row_shapes() {
        let v = serde_json::json!({ "row": { "surface": "汤面", "bottom": "汤底",
            "positive_guesses": ["g1", "g2"] } });
        let r = RawStory::from_value(&v).unwrap();
        assert_eq!(r.surface, "汤面");
        assert_eq!(r.positive_guesses.len(), 2);
        assert!(r.id.starts_with("ds-"));
    }

    #[test]
    fn parse_missing_fields_returns_none() {
        assert!(RawStory::from_value(&serde_json::json!({ "x": 1 })).is_none());
    }
}
