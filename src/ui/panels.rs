//! 五区面板渲染（纯函数：GameView + Rect → Buffer），见 docs/2-tui/01-界面设计.md §1.2。
//! 标题栏 / 汤面区 / 消息流 / 输入框 / 状态栏；不做任何 IO，不做判定。
// by AI.Coding

use crate::ui::{theme, GameView};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// 纵向四区切分：标题 1 + 汤面 4 + 对话流(余量) + 状态 1。
/// 「对话流」区把消息与输入框当作**同一条流**顺序排（消息… → 输入框上边框 → 输入行），
/// 输入行紧接最后一条消息（CLI 式紧随）。高度不足时返回 None（调用方走降级）。
pub fn split(area: Rect) -> Option<(Rect, Rect, Rect, Rect)> {
    if area.height < 10 {
        return None;
    }
    let [title, surface, flow, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);
    Some((title, surface, flow, status))
}

/// 标题栏：游戏名 + 题目星级/难度（1 行，无边框，反色强调）。
pub fn render_title(f: &mut Frame, area: Rect, v: &GameView) {
    let star = "★".repeat(v.difficulty.max(1) as usize);
    let line = Line::from(vec![
        Span::styled(format!(" {} ", v.game_title), Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD)),
        Span::raw("· "),
        Span::styled(v.puzzle_title.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(star, Style::new().fg(theme::PARTIAL)),
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

/// 条目 → 文本行。**纯单色**：只靠前缀区分，不着色、不加 emoji（见 01-界面设计.md §1.2、§2）。
pub fn feed_line(item: &crate::ui::FeedItem) -> Line<'static> {
    Line::from(format!("{}{}", item.kind.label(), item.text))
}

/// 输入行（含 busy spinner 态）。`❯ ` 之后即光标所在。
fn input_line(v: &GameView) -> Line<'static> {
    if v.busy {
        Line::from(vec![
            Span::styled(format!(" {} 思考中…", v.spinner()), Style::new().fg(theme::HINT)),
            Span::styled("（输入会保留）", Style::new().fg(theme::MUTED)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ❯ ", Style::new().fg(theme::BORDER).add_modifier(Modifier::BOLD)),
            Span::styled(v.input.clone(), Style::new().fg(theme::INPUT)),
        ])
    }
}

/// 对话流：把「消息… → 输入框上边框 → 输入行」当作**同一条流**顺序渲染。
/// - 内容未满可视高度：自上而下排列，输入行紧跟最后一条消息，下方留白（CLI 式紧随）。
/// - 内容超出：整体上滚，令输入行停在区域底部（最新始终可见）。
///
/// `scroll_offset` > 0 表示用户上翻查看历史（此时不定位光标）。
pub fn render_flow(f: &mut Frame, area: Rect, v: &GameView, scroll_offset: u16) {
    // 组装流：消息 + 分隔线（输入框上边框）+ 输入行。
    let mut lines: Vec<Line> = v.feed.iter().map(feed_line).collect();
    let sep = "─".repeat(area.width as usize);
    lines.push(Line::from(Span::styled(sep, Style::new().fg(theme::MUTED))));
    lines.push(input_line(v));

    let visible = area.height as usize;
    let total = lines.len();
    let end = total.saturating_sub(scroll_offset as usize);
    let start = end.saturating_sub(visible);
    let body: Vec<Line> = lines[start..end].to_vec();
    let input_row = body.len().saturating_sub(1); // 输入行相对行号

    let prompt_w = Line::from(" ❯ ").width() as u16;
    // 光标宽度按「光标之前的输入」计算（支持中途编辑）。
    let input_w = Line::from(&v.input[..v.input_cursor_byte()]).width() as u16;
    let cursor_x = {
        let max_x = area.x + area.width.saturating_sub(1);
        (area.x + prompt_w + input_w).min(max_x)
    };
    f.render_widget(Paragraph::new(body), area);

    // 仅贴底且非 busy 时显示光标（输入末尾，CJK 按显示宽度计）。
    if scroll_offset == 0 && !v.busy {
        f.set_cursor_position((cursor_x, area.y + input_row as u16));
    }
}

/// 状态栏：题名/难度/问数/进度/提示（1 行，暗灰）。
pub fn render_status(f: &mut Frame, area: Rect, v: &GameView) {
    let line = Line::from(Span::styled(
        format!(" {}", v.status_line()),
        Style::new().fg(theme::STATUS),
    ));
    f.render_widget(Paragraph::new(line), area);
}

/// 整屏渲染入口：四区一次画完。
pub fn render_all(f: &mut Frame, v: &GameView, scroll_offset: u16) {
    let Some((title, surface, flow, status)) = split(f.area()) else {
        return; // 高度不足，由调用方负责降级
    };
    render_title(f, title, v);
    render_surface(f, surface, v);
    render_flow(f, flow, v, scroll_offset);
    render_status(f, status, v);
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
            max_questions: 60,
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
        assert!(split(area).is_none());
        let area = Rect::new(0, 0, 80, 24);
        let (t, s, flow, st) = split(area).unwrap();
        assert_eq!(t.height, 1);
        assert_eq!(s.height, 4);
        assert!(flow.height >= 4);
        assert_eq!(st.height, 1);
        // 各区纵向恰好拼满。
        assert_eq!(t.height + s.height + flow.height + st.height, area.height);
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
        assert!(text.contains("进度1/5"), "状态栏缺进度: {text}");
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
        assert_eq!(msg_row + 1, border_row, "输入框上边框未紧随消息: {rows:?}");
        assert_eq!(border_row + 1, input_row, "输入行未在上边框之下: {rows:?}");
        // 汤面区在第 1–4 行，消息应从其下方（≥5）开始，而非被压到屏幕最底。
        assert!(msg_row >= 5, "消息未接在汤面下方: msg_row={msg_row}");
    }
}
