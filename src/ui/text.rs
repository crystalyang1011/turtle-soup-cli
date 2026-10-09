//! 纯文本降级渲染：非 TTY / 窄终端 / `--no-tui` 时使用。
//! 契约（01-界面设计.md §6）：**输出与阶段一 REPL 逐字节兼容**（ask 冒烟依赖）。
//! 与 TUI 共用 GameView 视图模型，只是渲染器不同。
// by AI.Coding

use crate::ui::{FeedKind, GameView};

/// 判定标签（与阶段一 `verdict_line`/`judgment_cn` 同源语义，TUI 化的纯文本形态）。
pub fn feed_prefix(kind: FeedKind) -> &'static str {
    kind.label()
}

/// 开局介绍（对应 TUI 的标题栏 + 汤面区）。
pub fn render_intro(v: &GameView) -> String {
    let mut out = String::new();
    out.push_str("\n=== 海龟汤 · 摸鱼版 ===\n");
    out.push_str(&format!(
        "题目: {}  (D{})\n",
        v.puzzle_title, v.difficulty
    ));
    out.push_str(&format!("\n【汤面】{}\n", v.surface));
    out.push_str("直接输入即提问 | /guess <推理> | /hint | /answer | /switch [id] | /hide | /list | /status | /quit\n");
    out.push_str(&format!(
        "(提示、猜底均不限次)  已问 {} 问，进度 {}/{}\n",
        v.question_count, v.hit, v.total
    ));
    out
}

/// 消息流条目 → 单行文本（非 TTY 下逐条打印用）。
pub fn render_feed_line(kind: FeedKind, text: &str) -> String {
    format!("{}{}", feed_prefix(kind), text)
}

/// 状态行（对应 TUI 状态栏）。
pub fn render_status(v: &GameView) -> String {
    format!("> {}", v.status_line())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> GameView {
        GameView {
            puzzle_title: "水与枪".into(),
            difficulty: 2,
            surface: "一个男人走进酒吧，要了一杯水。".into(),
            question_count: 0,
            hit: 0,
            total: 5,
            ..Default::default()
        }
    }

    /// 阶段一 render_intro 的逐字节兼容锚点（改输出必须同步 01-界面设计.md §6）。
    #[test]
    fn intro_matches_phase1_repl_output() {
        let expected = "\n=== 海龟汤 · 摸鱼版 ===\n\
            题目: 水与枪  (D2)\n\
            \n【汤面】一个男人走进酒吧，要了一杯水。\n\
            直接输入即提问 | /guess <推理> | /hint | /answer | /switch [id] | /hide | /list | /status | /quit\n\
            (提示、猜底均不限次)  已问 0 问，进度 0/5\n";
        assert_eq!(render_intro(&view()), expected);
    }

    #[test]
    fn feed_line_prefixes() {
        assert_eq!(render_feed_line(FeedKind::Question, "打嗝？"), "> 打嗝？");
        assert_eq!(render_feed_line(FeedKind::Yes, "是的"), "✓ 是的");
        assert_eq!(render_feed_line(FeedKind::No, "不是"), "✗ 不是");
        assert_eq!(render_feed_line(FeedKind::Irrelevant, "换个方向"), "- 换个方向");
        assert_eq!(render_feed_line(FeedKind::Hint, "换个角度"), "! 换个角度");
    }

    #[test]
    fn status_line_format() {
        assert!(render_status(&view()).contains("问 0"));
        assert!(!render_status(&view()).contains("/help"));
    }
}
