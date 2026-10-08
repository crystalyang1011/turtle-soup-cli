//! 判定引擎：评分、星级、命中/胜负、进度、提示选择（全部纯函数）。
//! 玩法规则见 docs/1-turtle-cli/01-游戏设计.md §3–§6。
//! 本模块严禁依赖 Tauri / 文件系统 / 网络，便于 headless 测试。
// by AI.Coding

use crate::models::{
    AppError, ErrorCode, HintLevel, JudgeVerdict, Judgment, Progress, Puzzle, ScoreResult,
};
use serde::{Deserialize, Serialize};

/// 主持人判定返回体（LLM → 本地），见 05 §1。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgmentResponse {
    pub judgment: Judgment,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub hit_facts: Vec<usize>,
}

/// 猜底裁判返回体（LLM → 本地），见 05 §2。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuessResponse {
    #[serde(default)]
    pub missed_facts: Vec<usize>,
    #[serde(default)]
    pub comment: String,
}

/// L2 事实提示折算成本（见 01 §5）。
pub const HINT_COST_L1: f32 = 0.5;
pub const HINT_COST_L2: f32 = 1.0;

/// 基准问数 par(difficulty) = 5 + 3·d，见 01 §3.1。
pub fn par(difficulty: u8) -> u32 {
    5 + 3 * difficulty.clamp(1, 5) as u32
}

/// 单局得分与星级，见 01 §3.1 / §3.2。
pub fn score(
    difficulty: u8,
    question_count: u32,
    hints_used: f32,
    guess_attempts_failed: u32,
) -> ScoreResult {
    let par = par(difficulty);
    let mut raw = 100f32;
    raw -= 2.0 * question_count.saturating_sub(par) as f32;
    raw -= 12.0 * hints_used;
    raw -= 8.0 * guess_attempts_failed as f32;
    raw = raw.max(0.0);
    let score = (raw * (1.0 + 0.2 * (difficulty.clamp(1, 5) as f32 - 1.0))).round() as u32;
    ScoreResult {
        score,
        stars: stars(hints_used, question_count, par),
    }
}

/// 星级：★★★ 0 提示且 ≤ par；★★ hints ≤ 1；★ 通关即得，见 01 §3.2。
pub fn stars(hints_used: f32, question_count: u32, par: u32) -> u8 {
    if hints_used == 0.0 && question_count <= par {
        3
    } else if hints_used <= 1.0 {
        2
    } else {
        1
    }
}

/// 去重合并命中序号（升序），见 03 §8。
pub fn merge_hit_facts(existing: &[usize], new: &[usize]) -> Vec<usize> {
    let mut v: Vec<usize> = existing.iter().chain(new.iter()).copied().collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 进度：命中数 / 总数。
pub fn progress(total: usize, hit_facts: &[usize]) -> Progress {
    Progress {
        hit: hit_facts.len(),
        total,
    }
}

/// 命中比率（无总数时记 0）。
pub fn hit_ratio(hit: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        hit as f32 / total as f32
    }
}

/// 所有 core 事实是否均被命中。命中序号越界视为未命中。
pub fn core_all_hit(puzzle: &Puzzle, hit_facts: &[usize]) -> bool {
    puzzle
        .key_facts
        .iter()
        .enumerate()
        .filter(|(_, f)| f.core)
        .all(|(i, _)| hit_facts.contains(&i))
}

/// 胜负判定：hit/total ≥ 0.7 且 core 全中，见 01 §4.1。
/// 用整数比较避免浮点误差：hit·10 ≥ 7·total。
pub fn is_win(puzzle: &Puzzle, hit_facts: &[usize]) -> bool {
    let total = puzzle.fact_count();
    if total == 0 {
        return false;
    }
    let valid = hit_facts.iter().filter(|i| **i < total).count();
    valid * 10 >= 7 * total && core_all_hit(puzzle, hit_facts)
}

/// 本轮猜底应判赢还是判负。
pub fn judge_verdict(puzzle: &Puzzle, hit_facts: &[usize]) -> JudgeVerdict {
    if is_win(puzzle, hit_facts) {
        JudgeVerdict::Win
    } else {
        JudgeVerdict::Lose
    }
}

/// 折算一次提示的 `hints_used` 增量。
pub fn hint_cost(level: HintLevel) -> f32 {
    match level {
        HintLevel::L1 => HINT_COST_L1,
        HintLevel::L2 => HINT_COST_L2,
    }
}

/// 尚未命中的事实文本（供方向提示 prompt）。
pub fn unhit_fact_texts(puzzle: &Puzzle, hit_facts: &[usize]) -> Vec<String> {
    puzzle
        .key_facts
        .iter()
        .enumerate()
        .filter(|(i, _)| !hit_facts.contains(i))
        .map(|(_, f)| f.text.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// LLM 输出解析
// ---------------------------------------------------------------------------

/// 从可能带 markdown 包裹/前后缀的文本中提取首个 JSON 对象。
/// 见 05 §5：`JSON.parse` 失败 → 提取首个 `{...}` 重试。
pub fn extract_json_block(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

/// 解析主持人判定 JSON。
pub fn parse_judgment(raw: &str) -> Result<JudgmentResponse, AppError> {
    if let Ok(v) = serde_json::from_str::<JudgmentResponse>(raw.trim()) {
        return Ok(normalize_judgment(v));
    }
    let block = extract_json_block(raw).ok_or_else(|| {
        AppError::new(ErrorCode::ParseFailed, "判定输出中未找到 JSON 对象")
    })?;
    let v: JudgmentResponse = serde_json::from_str(block)
        .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("判定 JSON 解析失败: {e}")))?;
    Ok(normalize_judgment(v))
}

fn normalize_judgment(mut v: JudgmentResponse) -> JudgmentResponse {
    v.reply = v.reply.trim().chars().take(60).collect();
    v.hit_facts.sort_unstable();
    v.hit_facts.dedup();
    v
}

/// 解析猜底判定 JSON。
pub fn parse_guess(raw: &str) -> Result<GuessResponse, AppError> {
    if let Ok(v) = serde_json::from_str::<GuessResponse>(raw.trim()) {
        return Ok(v);
    }
    let block = extract_json_block(raw)
        .ok_or_else(|| AppError::new(ErrorCode::ParseFailed, "猜底输出中未找到 JSON 对象"))?;
    serde_json::from_str(block)
        .map_err(|e| AppError::new(ErrorCode::ParseFailed, format!("猜底 JSON 解析失败: {e}")))
}

/// 由猜底裁判的 `missed_facts` 反推命中序号（本地计算，不采信 LLM 的比率）。
pub fn compute_hits(total: usize, missed: &[usize]) -> Vec<usize> {
    (0..total).filter(|i| !missed.contains(i)).collect()
}

/// 固定拒绝/兜底话术（见 05 §1、07 §2.2）。
pub const REFUSAL_REPLY: &str = "请用是/否问题还原真相。";

/// 输出层泄底检测：reply 若包含某条关键事实文本或汤底长片段，判为泄底。
/// 作为反泄底第三道防线（见 07 §2.3）。
pub fn reply_leaks(puzzle: &Puzzle, reply: &str) -> bool {
    let r = reply.trim();
    if r.is_empty() {
        return false;
    }
    // 命中整条事实（去掉过短的事实避免误伤）
    if puzzle
        .key_facts
        .iter()
        .any(|f| f.text.trim().chars().count() >= 4 && r.contains(f.text.trim()))
    {
        return true;
    }
    // 汤底中长度 ≥8 的连续片段被复述
    let truth: Vec<char> = puzzle.truth.chars().collect();
    if truth.len() >= 8 {
        for w in truth.windows(8) {
            let s: String = w.iter().collect();
            if s.chars().count() >= 8 && r.contains(&s) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::KeyFact;

    fn puzzle(n: usize, core: &[usize]) -> Puzzle {
        Puzzle {
            id: "p".into(),
            title: "p".into(),
            surface: "s".into(),
            truth: "t".into(),
            key_facts: (0..n)
                .map(|i| KeyFact {
                    text: format!("f{i}"),
                    core: core.contains(&i),
                })
                .collect(),
            difficulty: 2,
            tags: vec![],
            source: "builtin".into(),
            created_at: 0,
        }
    }

    #[test]
    fn par_matches_doc() {
        assert_eq!(par(1), 8);
        assert_eq!(par(2), 11);
        assert_eq!(par(3), 14);
        assert_eq!(par(4), 17);
        assert_eq!(par(5), 20);
    }

    #[test]
    fn score_basic_and_difficulty_multiplier() {
        // 5 facts, 0 hints, 0 failed guesses, question=par → raw 100
        let s = score(1, 8, 0.0, 0);
        assert_eq!(s.score, 100);
        assert_eq!(s.stars, 3);
        // difficulty 5 → ×1.8
        let s5 = score(5, 20, 0.0, 0);
        assert_eq!(s5.score, 180);
    }

    #[test]
    fn score_penalties_and_floor() {
        // 超 par 3 问罚 6；1 次 L1 提示罚 6；1 次猜底失败罚 8 → 100-6-6-8=80
        let s = score(1, 11, 0.5, 1);
        assert_eq!(s.score, 80);
        // 重罚到 0 不为负
        let s = score(1, 100, 20.0, 10);
        assert_eq!(s.score, 0);
    }

    #[test]
    fn stars_thresholds() {
        assert_eq!(stars(0.0, 8, 8), 3);
        assert_eq!(stars(0.0, 9, 8), 2); // 超 par 掉星
        assert_eq!(stars(1.0, 8, 8), 2);
        assert_eq!(stars(1.5, 8, 8), 1);
    }

    #[test]
    fn win_requires_ratio_and_core() {
        // 5 facts, core = {0,4}
        let p = puzzle(5, &[0, 4]);
        // hit 4/5 = 0.8 但缺 core 4 → 输
        assert!(!is_win(&p, &[0, 1, 2, 3]));
        // 命中 core 但只有 3/5 = 0.6 → 输
        assert!(!is_win(&p, &[0, 4, 1]));
        // 4/5 且 core 全中 → 赢
        assert!(is_win(&p, &[0, 4, 1, 2]));
    }

    #[test]
    fn win_boundary_four_facts_needs_three() {
        let p = puzzle(4, &[0]);
        assert!(!is_win(&p, &[0, 1])); // 2/4 = 0.5
        assert!(is_win(&p, &[0, 1, 2])); // 3/4 = 0.75
    }

    #[test]
    fn merge_and_progress() {
        let m = merge_hit_facts(&[2, 0], &[0, 3]);
        assert_eq!(m, vec![0, 2, 3]);
        let p = progress(5, &m);
        assert_eq!(p.hit, 3);
        assert_eq!(p.total, 5);
    }

    #[test]
    fn parse_judgment_plain_and_fenced() {
        let raw = r#"{"judgment":"yes","reply":"是","hit_facts":[0,0,2]}"#;
        let j = parse_judgment(raw).unwrap();
        assert_eq!(j.judgment, Judgment::Yes);
        assert_eq!(j.hit_facts, vec![0, 2]); // 去重

        let fenced = "```json\n{\"judgment\":\"no\",\"reply\":\"否\"}\n```";
        let j = parse_judgment(fenced).unwrap();
        assert_eq!(j.judgment, Judgment::No);
    }

    #[test]
    fn parse_judgment_error_when_garbage() {
        assert_eq!(
            parse_judgment("完全不是 JSON").unwrap_err().code,
            ErrorCode::ParseFailed
        );
    }

    #[test]
    fn compute_hits_from_missed() {
        assert_eq!(compute_hits(5, &[1, 3]), vec![0, 2, 4]);
    }

    #[test]
    fn parse_guess_and_verdict_flow() {
        let p = puzzle(5, &[0, 4]);
        let g = parse_guess(r#"{"missed_facts":[2],"comment":"差一点"}"#).unwrap();
        let hits = compute_hits(p.fact_count(), &g.missed_facts);
        assert_eq!(hits, vec![0, 1, 3, 4]);
        assert_eq!(judge_verdict(&p, &hits), JudgeVerdict::Win);
    }

    #[test]
    fn leak_detection_catches_fact_and_truth() {
        let mut p = puzzle(5, &[0, 4]);
        p.key_facts[1].text = "他其实在打嗝".into();
        p.truth = "男人在打嗝要水是为了止嗝老板开枪吓他".into();
        assert!(reply_leaks(&p, "他其实在打嗝"));
        assert!(reply_leaks(&p, "男人在打嗝要水是为了止嗝"));
        assert!(!reply_leaks(&p, "是"));
        assert!(!reply_leaks(&p, "无关"));
    }
}
