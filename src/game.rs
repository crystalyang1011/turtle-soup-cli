//! 对局编排层：把 llm + engine + session 串起来，供 CLI 与 Tauri commands 共用，
//! 避免两端各写一遍（与 04 §7「单一事实源」同一原则）。
// by AI.Coding

use crate::config::AppConfig;
use crate::engine::{self, JudgmentResponse};
use crate::llm::{ChatMessage, LlmClient};
use crate::models::{
    AppError, AskHostResult, ErrorCode, HintLevel, HintResult, JudgeGuessResult, Puzzle, Session,
};
use crate::prompts;
use crate::session::PuzzleStore;

/// 单局最大提问数（见 02 §3 token 预算）。
pub const MAX_QUESTIONS: u32 = 60;

/// 对局服务：持有 LLM 客户端与题库。
#[derive(Clone)]
pub struct GameService {
    pub llm: LlmClient,
    pub store: PuzzleStore,
}

impl GameService {
    /// 由配置 + API Key 构造。
    pub fn new(cfg: &AppConfig, api_key: impl Into<String>) -> Result<Self, AppError> {
        Ok(Self {
            llm: LlmClient::new(cfg, api_key)?,
            store: PuzzleStore::load()?,
        })
    }

    /// 取题（按 id 或首题）。
    pub fn pick_puzzle(&self, id: Option<&str>) -> Result<Puzzle, AppError> {
        match id {
            Some(i) => self.store.get(i).cloned().ok_or_else(|| {
                AppError::new(ErrorCode::PuzzleNotFound, format!("题库中不存在题目 {i}"))
            }),
            None => self
                .store
                .all()
                .into_iter()
                .next()
                .cloned()
                .ok_or_else(|| AppError::new(ErrorCode::PuzzleNotFound, "题库为空")),
        }
    }

    /// 构造主持人对话历史：system + 逐条 user/assistant。
    fn build_history(puzzle: &Puzzle, session: &Session) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(prompts::host_system(puzzle))];
        for m in &session.messages {
            match m.role {
                crate::models::Role::Player => msgs.push(ChatMessage::user(m.text.clone())),
                crate::models::Role::Host => msgs.push(ChatMessage::assistant(m.text.clone())),
            }
        }
        msgs
    }

    /// 一次问答判定（含反泄底输出校验）。见 01 §2、07 §2.3。
    pub async fn ask_host(
        &self,
        puzzle: &Puzzle,
        session: &mut Session,
        text: &str,
    ) -> Result<AskHostResult, AppError> {
        if session.status.is_finished() {
            return Err(AppError::new(ErrorCode::InvalidState, "本局已结束"));
        }
        if session.question_count >= MAX_QUESTIONS {
            return Err(AppError::new(ErrorCode::InvalidState, "提问数已达上限"));
        }
        let text = sanitize_input(text);
        session.push_player(text);
        let history = Self::build_history(puzzle, session);

        let raw = self.llm.complete(&self.llm.model, 0.0, &history).await?;
        let JudgmentResponse { judgment, mut reply, hit_facts } =
            engine::parse_judgment(&raw).inspect_err(|_| {
                // 解析失败的原始输出已在 llm 层记落，这里补上下文。
                crate::log_error!(
                    "engine",
                    "判定解析失败 session={}: raw={:?}",
                    session.id,
                    raw.chars().take(300).collect::<String>()
                );
            })?;

        // 输出层泄底检测：命中则替换为固定话术。
        if engine::reply_leaks(puzzle, &reply) {
            crate::log_warn!("engine", "判定回复疑似泄底，已拦截 session={}", session.id);
            reply = engine::REFUSAL_REPLY.to_string();
        }

        let progress = session.apply_judgment(puzzle.fact_count(), judgment, reply.clone(), hit_facts.clone());
        crate::log_info!("game", "ask session={} judgment={:?} 进度={}/{}",
            session.id, judgment, progress.hit, progress.total);
        Ok(AskHostResult { judgment, reply, hit_facts, progress })
    }

    /// 一次猜底判定。命中/胜负由本地计算（见 01 §4.1）。
    pub async fn judge_guess(
        &self,
        puzzle: &Puzzle,
        session: &mut Session,
        guess: &str,
    ) -> Result<JudgeGuessResult, AppError> {
        if session.status.is_finished() {
            return Err(AppError::new(ErrorCode::InvalidState, "本局已结束"));
        }
        let guess = sanitize_input(guess);
        session.push_player(guess.clone());
        let msgs = [
            ChatMessage::system(prompts::guess_system(puzzle, &guess)),
            ChatMessage::user(guess.clone()),
        ];
        let raw = self.llm.complete(&self.llm.model, 0.0, &msgs).await?;
        let g = engine::parse_guess(&raw)?;
        let hits = engine::compute_hits(puzzle.fact_count(), &g.missed_facts);
        Ok(session.apply_guess(puzzle, hits, g.comment))
    }

    /// 使用一次提示。L1 走模型（不泄底），L2 直接取事实原文。见 01 §5。
    pub async fn use_hint(
        &self,
        puzzle: &Puzzle,
        session: &mut Session,
        level: HintLevel,
    ) -> Result<HintResult, AppError> {
        if session.status.is_finished() {
            return Err(AppError::new(ErrorCode::InvalidState, "本局已结束"));
        }
        let used = session.hint_levels.len() as u32;
        if used >= engine::max_hints(puzzle.fact_count()) {
            return Err(AppError::new(ErrorCode::InvalidState, "提示次数已用完"));
        }
        match level {
            HintLevel::L2 => {
                let idx = engine::pick_hint_fact(puzzle, &session.hit_facts).ok_or_else(|| {
                    AppError::new(ErrorCode::InvalidState, "所有事实均已命中，无需提示")
                })?;
                let text = puzzle.key_facts[idx].text.clone();
                session.apply_hint(HintLevel::L2, text.clone(), Some(idx));
                Ok(HintResult { text, hit_fact: Some(idx) })
            }
            HintLevel::L1 => {
                let unhit = engine::unhit_fact_texts(puzzle, &session.hit_facts);
                let sys = prompts::hint_l1_system(puzzle, &unhit);
                let raw = self
                    .llm
                    .complete(
                        &self.llm.model,
                        0.7,
                        &[ChatMessage::system(sys), ChatMessage::user("给一句提示")],
                    )
                    .await;
                // 生成失败降级为固定不泄底文案（见 05 §3）。
                let text = match raw {
                    Ok(t) if !t.trim().is_empty() && !engine::reply_leaks(puzzle, &t) => {
                        t.trim().chars().take(25).collect()
                    }
                    _ => "换个角度想想：他的目的可能不是表面那样。".to_string(),
                };
                session.apply_hint(HintLevel::L1, text.clone(), None);
                Ok(HintResult { text, hit_fact: None })
            }
        }
    }
}

/// 输入清洗：截断 500 字符、去控制字符（见 05 §5、01 §7）。
pub fn sanitize_input(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(500)
        .collect();
    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_truncates_and_strips_controls() {
        let long = "a".repeat(600);
        assert_eq!(sanitize_input(&long).chars().count(), 500);
        assert_eq!(sanitize_input("  hi\u{0007} "), "hi");
    }
}
