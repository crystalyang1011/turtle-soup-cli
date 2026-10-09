//! 五区面板渲染（纯函数：GameView + Rect → Buffer），见 docs/2-tui/01-界面设计.md §1.2。
//! 标题栏 / 汤面区 / 消息流 / 输入框 / 状态栏；不做任何 IO，不做判定。
// by AI.Coding

use crate::ui::{theme, GameView};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// 汤面区最大高度（行），防止超长汤面吃满屏幕。
const SURFACE_MAX_LINES: usize = 6;

/// 汤面区高度 = 汤面（含 `【汤面】` 前缀）按宽度换行后的**实际行数**，夹在 [1, 6]。
/// 用 `Line::width()`（显示宽度）估算：CJK 无空格按字符换行，与此处 `Wrap` 行为一致。
pub fn surface_height(surface: &str, area_width: u16) -> u16 {
    let text = format!("【汤面】{surface}");
    let inner_w = area_width.saturating_sub(1).max(1) as usize; // 左侧边框占 1 列
    let total_w = Line::from(text).width();
    let lines = total_w.div_ceil(inner_w);
    lines.clamp(1, SURFACE_MAX_LINES) as u16
}

/// 纵向切分（自上而下）：① 标题 1 + ② 汤面(surface_h) + 间隔 1 + ③ 信息与操作 2 + 间隔 1 + 对话流(余量)。
/// 「信息与操作」区两行：动态进度 + 完整命令列表（见 01-界面设计.md §1.2）。
/// 高度不足以容纳时返回 None（调用方走降级）。
pub fn split(area: Rect, surface_h: u16) -> Option<(Rect, Rect, Rect, Rect)> {
    let surface_h = surface_h.max(1);
    // 需要容纳：标题 1 + 汤面 surface_h + 间隔 1 + 信息与操作 2 + 间隔 1 + 对话流至少 4 行。
    if area.height < 1 + surface_h + 1 + 2 + 1 + 4 {
        return None;
    }
    let [title, surface, _g1, info, _g2, flow] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(surface_h),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(4),
    ])
    .areas(area);
    Some((title, surface, info, flow))
}

/// 标题栏：`游戏名 · 题目标题  Lv.难度 [id]`（1 行）。
pub fn render_title(f: &mut Frame, area: Rect, v: &GameView) {
    let line = Line::from(vec![
        Span::styled(format!(" {} ", v.game_title), Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD)),
        Span::styled("· ", Style::new().fg(theme::MUTED)),
        Span::styled(v.puzzle_title.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  Lv.{}", v.difficulty), Style::new().fg(theme::PARTIAL)),
        Span::styled(format!(" [{}]", v.puzzle_id), Style::new().fg(theme::MUTED)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// 汤面区：整局常驻谜面，自动换行，最多展示区域内文字（超出截断，完整汤面见开局 / 纯文本模式）。
pub fn render_surface(f: &mut Frame, area: Rect, v: &GameView) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme::BORDER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let para = Paragraph::new(Line::from(Span::styled(
        format!("【汤面】{}", v.surface),
        Style::new().fg(theme::SURFACE).add_modifier(Modifier::BOLD),
    )))
    .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(para, inner);
}

/// 条目 → 单行文本（前缀 + 内容）。**纯单色**：只靠前缀区分（见 01-界面设计.md §1.2、§2）。
fn feed_text(item: &crate::ui::FeedItem) -> String {
    format!("{}{}", item.kind.label(), item.text)
}

/// 条目 → 单行 `Line`（未换行；供测试与短文本使用）。
pub fn feed_line(item: &crate::ui::FeedItem) -> Line<'static> {
    Line::from(feed_text(item))
}

/// 按**显示宽度**把文本硬换行成多段（CJK 双宽按 2 计；`\n` 强制断行）。
/// 自评分行（不依赖 ratatui 的 unstable `line_count`），使「一行 = 屏幕一行」，滚动与光标定位可控。
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
            continue;
        }
        let cw = Line::from(ch.to_string()).width().max(1);
        if cur_w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    out.push(cur);
    out
}

/// 对话流：把「消息… → 输入框上边框 → 输入行」当作**同一条流**顺序渲染；
/// 每条消息与输入行都按显示宽度**预换行**为多行（长汤底自动折行）。
/// - 内容未满可视高度：自上而下排列，输入行紧跟最后一条消息，下方留白（CLI 式紧随）。
/// - 内容超出：整体上滚，令输入行停在区域底部（最新始终可见）。
///
/// `scroll_offset` > 0 表示用户上翻查看历史（此时不定位光标）。
pub fn render_flow(f: &mut Frame, area: Rect, v: &GameView, scroll_offset: u16) {
    let width = area.width as usize;
    // 组装流：消息（每条按宽度换行，条目之间留一空行） + 分隔线 + 输入行。
    let mut lines: Vec<Line> = Vec::new();
    for item in &v.feed {
        for seg in wrap_text(&feed_text(item), width) {
            lines.push(Line::from(seg));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled("─".repeat(width), Style::new().fg(theme::MUTED))));

    // 输入区（busy 为 spinner；否则 `❯ ` + 输入），同样按宽度换行。
    let input_start = lines.len();
    let input_text = if v.busy {
        format!(" {} 思考中…", v.spinner())
    } else {
        format!(" ❯ {}", v.input)
    };
    for seg in wrap_text(&input_text, width) {
        lines.push(Line::from(seg));
    }

    let visible = area.height as usize;
    let total = lines.len();
    let end = total.saturating_sub(scroll_offset as usize);
    let start = end.saturating_sub(visible);
    let body: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(body), area);

    // 仅贴底且非 busy 时显示光标：定位到「`❯ ` + 光标前输入」换行后的最后一段末尾。
    if scroll_offset == 0 && !v.busy {
        let before = format!(" ❯ {}", &v.input[..v.input_cursor_byte()]);
        let bsegs = wrap_text(&before, width);
        let cursor_idx = input_start + bsegs.len().saturating_sub(1);
        let row = cursor_idx.saturating_sub(start) as u16;
        let xw = bsegs.last().map(|s| Line::from(s.as_str()).width()).unwrap_or(0) as u16;
        let x = (area.x + xw).min(area.x + area.width.saturating_sub(1));
        f.set_cursor_position((x, area.y + row));
    }
}

/// 信息与操作区（2 行）：动态进度 + 完整命令列表（暗灰）。
pub fn render_status(f: &mut Frame, area: Rect, v: &GameView) {
    let lines = vec![
        Line::from(Span::styled(format!(" {}", v.status_line()), Style::new().fg(theme::STATUS))),
        Line::from(Span::styled(
            format!(" {}", crate::ui::COMMANDS_HINT),
            Style::new().fg(theme::MUTED),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

/// 整屏渲染入口：按 ①标题 → ②汤面 → ③信息与操作 → 对话流 顺序绘制。
pub fn render_all(f: &mut Frame, v: &GameView, scroll_offset: u16) {
    let area = f.area();
    let surface_h = surface_height(&v.surface, area.width);
    let Some((title, surface, info, flow)) = split(area, surface_h) else {
        return; // 高度不足，由调用方负责降级
    };
    render_title(f, title, v);
    render_surface(f, surface, v);
    render_status(f, info, v);
    render_flow(f, flow, v, scroll_offset);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{FeedItem, FeedKind};
    use ratatui::backend::Backend;

    fn view() -> GameView {
        let mut v = GameView {
            game_title: "海龟汤".into(),
            puzzle_title: "水与枪".into(),
            difficulty: 2,
            surface: "一个男人走进酒吧".into(),
            question_count: 3,
            hit: 1,
            total: 5,
            hints_used: 1,
            ..Default::default()
        };
        v.push(FeedKind::Question, "他在打嗝吗");
        v.push(FeedKind::Yes, "是的");
        v
    }

    #[test]
    fn split_rejects_short_terminal() {
        let area = Rect::new(0, 0, 80, 9);
        assert!(split(area, 4).is_none());
        let area = Rect::new(0, 0, 80, 24);
        let (t, s, info, flow) = split(area, 4).unwrap();
        assert_eq!(t.height, 1);
        assert_eq!(s.height, 4);
        assert_eq!(info.height, 2);
        assert!(flow.height >= 4);
        // 各区 + 两处间隔（各 1 行）纵向恰好拼满。
        assert_eq!(t.height + s.height + 1 + info.height + 1 + flow.height, area.height);
    }

    #[test]
    fn surface_height_tracks_wrapped_lines() {
        // 单行汤面 → 1 行；长汤面按换行增长；上限 6 行。
        assert_eq!(surface_height("一个男人走进酒吧", 80), 1);
        let long = "啊".repeat(400);
        assert_eq!(surface_height(&long, 80), 6);
        // 宽度越窄行数越多（单调不减）。
        assert!(surface_height("一个男人走进酒吧要了一杯水", 20) >= surface_height("一个男人走进酒吧要了一杯水", 80));
    }

    #[test]
    fn wrap_text_breaks_by_display_width() {
        // 宽度 10：每个 CJK 宽 2，7 个字 = 14 → 折成 2 行，且每行不超宽。
        let segs = wrap_text(&"啊".repeat(7), 10);
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| Line::from(s.as_str()).width() <= 10));
        assert_eq!(wrap_text("abc", 10).len(), 1);
        // `\n` 强制断行。
        assert_eq!(wrap_text("a\nb", 10).len(), 2);
    }

    #[test]
    fn long_truth_wraps_into_multiple_rows() {
        // 长汤底应折行（多行都含内容），而不是被截断。
        let mut v = view();
        v.feed.clear();
        v.push(FeedKind::Truth, "很长很长的汤底".repeat(20));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 24)).unwrap();
        terminal.draw(|f| render_all(f, &v, 0)).unwrap();
        let buf = terminal.backend().buffer();
        let content_rows = (0..24)
            .filter(|&y| {
                let s: String = (0..40)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or("").to_string())
                    .collect();
                s.contains('长')
            })
            .count();
        assert!(content_rows >= 2, "长汤底未换行: rows={content_rows}");
    }

    #[test]
    fn feed_line_is_monochrome_with_prefix() {
        let l = feed_line(&FeedItem { kind: FeedKind::Yes, text: "是的".into() });
        let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "✓ 是的");
        // 纯单色：不着色（P0 前缀区分）。
        assert_eq!(l.spans[0].style.fg, None);
        let sys = feed_line(&FeedItem { kind: FeedKind::System, text: "已挂起".into() });
        assert_eq!(sys.spans[0].style.fg, None);
    }

    /// 把 TestBackend 的 buffer 拼成可断言文本。
    /// 注意：ratatui 对宽字符（CJK）会在其后占用一个「续格」，内容为空格，
    /// 故这里去掉所有空白再比对，避免 `水 与 枪` 这种伪分隔。
    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let raw: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        raw.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn render_all_draws_into_buffer_without_panic() {
        // 离屏 Buffer 上渲染，验证五区无 panic 且关键文本落位（text 已去空白）。
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_all(f, &view(), 0)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("水与枪"), "标题缺题目名: {text}");
        assert!(text.contains("进度1/5"), "信息行缺进度: {text}");
        assert!(text.contains("/guess"), "操作行缺命令列表: {text}");
        assert!(text.contains("❯"), "输入框缺提示符: {text}");
        assert!(text.contains("【汤面】"), "汤面区缺标题: {text}");
    }

    #[test]
    fn busy_replaces_prompt_with_spinner() {
        let mut v = view();
        v.busy = true;
        v.spinner_frame = 1;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_all(f, &v, 0)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("思考中"), "busy 态缺 spinner 文案: {text}");
        assert!(!text.contains("❯"), "busy 态不应显示输入提示符: {text}");
    }

    #[test]
    fn cursor_tracks_input_end() {
        // 光标应随输入增长右移，且 busy 态不定位（保持隐藏）。
        let mut v = view();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_all(f, &v, 0)).unwrap();
        let x_empty = terminal.backend_mut().get_cursor_position().unwrap().x;

        v.input = "打嗝吗".into();
        v.move_end();
        terminal.draw(|f| render_all(f, &v, 0)).unwrap();
        let x_filled = terminal.backend_mut().get_cursor_position().unwrap().x;
        assert!(x_filled > x_empty, "光标未随输入右移: {x_empty} -> {x_filled}");
        assert!(x_filled < 80, "光标越界: {x_filled}");
    }

    #[test]
    fn flow_puts_input_right_after_last_message() {
        // CLI 式紧随：最后一条消息的下一行是输入框上边框，再下一行是输入行；且都在汤面下方。
        let mut v = view();
        v.feed.clear();
        v.push(FeedKind::Yes, "第一条回复");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_all(f, &v, 0)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or("").to_string())
                    .collect::<String>()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect()
            })
            .collect();
        let msg_row = rows.iter().position(|r| r.contains("第一条回复")).unwrap();
        let border_row = rows.iter().position(|r| r.contains('─')).unwrap();
        let input_row = rows.iter().position(|r| r.contains('❯')).unwrap();
        // 消息与输入框之间留一空行：消息 → 空行 → 上边框 → 输入行。
        assert_eq!(msg_row + 2, border_row, "输入框上边框未与消息留一行间距: {rows:?}");
        assert_eq!(border_row + 1, input_row, "输入行未在上边框之下: {rows:?}");
        // 消息应紧接在信息与操作区下方：标题1 + 汤面 sh + 间隔1 + 信息2 + 间隔1。
        let sh = surface_height(&v.surface, 80) as usize;
        assert_eq!(msg_row, 1 + sh + 1 + 2 + 1, "消息未接在信息区下方: msg_row={msg_row}, surface_h={sh}");
    }
}
