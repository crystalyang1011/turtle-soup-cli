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

/// 结构化输出解析失败时的纠正指令：`temperature=0` 重调一次（见 05 §5）。
const PARSE_REPAIR_HINT: &str =
    "你的上一条回复不是合法 JSON。请仅输出一个 JSON 对象，不要包含任何解释、markdown 代码块或括号外的文字。";

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

    /// 调 LLM 并解析结构化（JSON）输出。
    ///
    /// 首次解析失败时按 05 §5 追加纠正指令、以 `temperature=0` **重调一次**；
    /// 仍失败则返回**首次**错误（原始 raw 已由 llm 层记日志），不静默降级。
    async fn complete_parsed<T>(
        &self,
        messages: &[ChatMessage],
        parse: impl Fn(&str) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let raw = self.llm.complete(&self.llm.model, 0.0, messages).await?;
        match parse(&raw) {
            Ok(v) => Ok(v),
            Err(first) => {
                // 首次不是合法 JSON：记 INFO（仅落文件，不打扰界面），追加纠正指令重调一次。
                crate::log_info!(
                    "engine",
                    "结构化解析失败，追加纠正指令重调一次: raw={:?}",
                    raw.chars().take(300).collect::<String>()
                );
                let mut retry = messages.to_vec();
                retry.push(ChatMessage::user(PARSE_REPAIR_HINT));
                let raw2 = self.llm.complete(&self.llm.model, 0.0, &retry).await?;
                match parse(&raw2) {
                    Ok(v) => Ok(v),
                    Err(_) => {
                        crate::log_error!(
                            "engine",
                            "重调后仍解析失败: raw={:?}",
                            raw2.chars().take(300).collect::<String>()
                        );
                        Err(first)
                    }
                }
            }
        }
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
        let text = sanitize_input(text);
        let rollback_at = session.messages.len();
        session.push_player(text);
        let history = Self::build_history(puzzle, session);

        // 解析失败会带纠正指令重调一次（见 complete_parsed / 05 §5）。
        let parsed = self.complete_parsed(&history, engine::parse_judgment).await;
        let JudgmentResponse { judgment, mut reply, hit_facts } = match parsed {
            Ok(v) => v,
            Err(e) => {
                // 判定失败：回滚未获回应的提问，避免历史残留悬空 user 消息（见 03 §8）。
                session.messages.truncate(rollback_at);
                return Err(e);
            }
        };

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
        let rollback_at = session.messages.len();
        session.push_player(guess.clone());
        let msgs = [
            ChatMessage::system(prompts::guess_system(puzzle, &guess)),
            ChatMessage::user(guess.clone()),
        ];
        let g = match self.complete_parsed(&msgs, engine::parse_guess).await {
            Ok(v) => v,
            Err(e) => {
                session.messages.truncate(rollback_at);
                return Err(e);
            }
        };
        let hits = engine::compute_hits(puzzle.fact_count(), &g.missed_facts);
        Ok(session.apply_guess(puzzle, hits, g.comment))
    }

    /// 使用一次方向提示（不泄底，次数不限，见 01 §5）。
    pub async fn use_hint(
        &self,
        puzzle: &Puzzle,
        session: &mut Session,
    ) -> Result<HintResult, AppError> {
        if session.status.is_finished() {
            return Err(AppError::new(ErrorCode::InvalidState, "本局已结束"));
        }
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
