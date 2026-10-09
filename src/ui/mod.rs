//! TUI 平台壳（阶段二）：全屏面板渲染 + 事件循环 + 纯文本降级。
//! 设计契约见 docs/2-tui/00-总览.md、01-界面设计.md。
//!
//! 分层铁律（AGENTS §3.2）：本模块**只消费视图 DTO**，不 import `llm`、不触碰 `truth`
//! （结算 DTO 显式携带时除外）；任何判定逻辑都不得写在这里。
// by AI.Coding

pub mod panels;
pub mod text;
pub mod tui;

/// 消息流单条目（不可变视图，渲染层只读）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    /// 条目类型决定前缀图标与配色（见 01-界面设计.md §1.2）。
    pub kind: FeedKind,
    /// 展示文本（不含图标；图标由渲染层按 kind 附加）。
    pub text: String,
}

/// 消息流条目类型（配色契约见 01-界面设计.md §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    /// 玩家提问（❓，默认前景色）。
    Question,
    /// 判定【是】（✅，绿）。
    Yes,
    /// 判定【否】（❌，红）。
    No,
    /// 判定【一半】（🟡，黄）。
    Partial,
    /// 判定【无关】（⚪，暗灰）。
    Irrelevant,
    /// 提示（💡，黄）。
    Hint,
    /// 系统事件（暗灰）：挂起、切换、/list 输出等。
    System,
    /// 结算汤底（青色）——仅结算后由 DTO 显式携带。
    Truth,
}

impl FeedKind {
    /// 渲染前缀（纯单色、单宽、无 emoji；只靠前缀区分，见 01-界面设计.md §1.2）。
    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Question => "> ",
            FeedKind::Yes => "✓ ",
            FeedKind::No => "✗ ",
            FeedKind::Partial => "~ ",
            FeedKind::Irrelevant => "- ",
            FeedKind::Hint => "! ",
            FeedKind::System => "",
            FeedKind::Truth => "# ",
        }
    }
}

/// 一局游戏的完整视图状态（装配层从 Session / 对局结果映射而来）。
#[derive(Debug, Clone, Default)]
pub struct GameView {
    /// 标题栏左侧：游戏名。
    pub game_title: String,
    /// 题目标题（如「水与枪」）。
    pub puzzle_title: String,
    /// 题目 id（如 `classic-001`，状态栏展示用）。
    pub puzzle_id: String,
    /// 难度 D1–D5（0 表示未开局）。
    pub difficulty: u8,
    /// 汤面（整局常驻）。
    pub surface: String,
    /// 消息流条目（时间序）。
    pub feed: Vec<FeedItem>,
    /// 已提问数（仅记录，不设上限）。
    pub question_count: u32,
    /// 进度命中数。
    pub hit: usize,
    /// 进度总数。
    pub total: usize,
    /// 输入缓冲。
    pub input: String,
    /// 输入光标（**字符**索引，0..=input 字符数）。
    pub input_cursor: usize,
    /// LLM 等待中（输入框转 spinner）。
    pub busy: bool,
    /// busy 态 spinner 当前帧索引。
    pub spinner_frame: usize,
    /// 已用提示次数。
    pub hints_used: u32,
}

impl GameView {
    /// 追加一条消息流条目。
    pub fn push(&mut self, kind: FeedKind, text: impl Into<String>) {
        self.feed.push(FeedItem { kind, text: text.into() });
    }

    /// 光标处对应的字节下标（供渲染计算显示宽度）。
    pub fn input_cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.input_cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// 光标处插入字符。
    pub fn insert_char(&mut self, c: char) {
        let idx = self.input_cursor_byte();
        self.input.insert(idx, c);
        self.input_cursor += 1;
    }

    /// Backspace：删除光标前一个字符。
    pub fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = self.input_cursor_byte();
        let start = self.input[..end]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
    }

    /// Delete：删除光标处字符。
    pub fn delete(&mut self) {
        if self.input_cursor >= self.input.chars().count() {
            return;
        }
        let start = self.input_cursor_byte();
        let end = self.input[start..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| start + i)
            .unwrap_or(self.input.len());
        self.input.replace_range(start..end, "");
    }

    pub fn move_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.input_cursor < self.input.chars().count() {
            self.input_cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.input_cursor = self.input.chars().count();
    }

    /// 清空输入（Esc）；光标复位。
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }

    /// 消息流容量上限：超出时从头部丢弃（见 01-界面设计.md §1.2；完整历史在会话文件里）。
    pub const FEED_CAP: usize = 200;

    /// 状态栏文本（动态进度；题目身份在标题栏，见 01-界面设计.md §1.2）。
    pub fn status_line(&self) -> String {
        format!(
            "问 {} · 进度 {}/{} · 提示 {}",
            self.question_count, self.hit, self.total, self.hints_used
        )
    }

    /// busy 态 spinner 帧（CJK 宽度友好的盲文字符）。
    pub fn spinner(&self) -> &'static str {
        const FRAMES: [&str; 8] = ["◐", "◓", "◑", "◒", "◓", "◑", "◒", "◐"];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }
}

/// 对局内命令帮助行（状态区第二行，见 01-界面设计.md §1.2）。
pub const COMMANDS_HINT: &str =
    "/guess <推理>  /hint  /answer  /switch [id]  /hide  /list  /status  /quit";

/// 主题：ANSI 16 色安全集（见 01-界面设计.md §2）。
pub mod theme {
    use ratatui::style::Color;

    pub const BORDER: Color = Color::Cyan;
    pub const TITLE: Color = Color::Cyan;
    pub const SURFACE: Color = Color::White;
    pub const YES: Color = Color::Green;
    pub const NO: Color = Color::Red;
    pub const PARTIAL: Color = Color::Yellow;
    pub const HINT: Color = Color::Yellow;
    pub const MUTED: Color = Color::DarkGray;
    pub const TRUTH: Color = Color::Cyan;
    pub const INPUT: Color = Color::White;
    pub const STATUS: Color = Color::DarkGray;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_contains_key_stats() {
        let v = GameView {
            puzzle_title: "水与枪".into(),
            puzzle_id: "classic-001".into(),
            difficulty: 2,
            question_count: 3,
            hit: 1,
            total: 5,
            hints_used: 2,
            ..Default::default()
        };
        let s = v.status_line();
        // 提问数只记录、不显示上限。
        assert!(s.contains("问 3"));
        assert!(!s.contains("问 3/"));
        assert!(s.contains("进度 1/5"));
        assert!(s.contains("提示 2"));
        assert!(!s.contains("/help"));
        // 题目身份已移到标题栏，状态栏不再重复。
        assert!(!s.contains("水与枪"));
        assert!(!s.contains("classic-001"));
    }

    #[test]
    fn push_appends_feed() {
        let mut v = GameView::default();
        v.push(FeedKind::Question, "他在打嗝吗");
        v.push(FeedKind::Yes, "是的");
        assert_eq!(v.feed.len(), 2);
        assert_eq!(v.feed[0].text, "他在打嗝吗");
        assert_eq!(v.feed[1].kind, FeedKind::Yes);
    }

    #[test]
    fn labels_cover_all_kinds() {
        // 每种条目都有非空前缀/标签（System 除外，由渲染层自行处理）。
        for (k, expect_empty) in [
            (FeedKind::Question, false),
            (FeedKind::Yes, false),
            (FeedKind::No, false),
            (FeedKind::Partial, false),
            (FeedKind::Irrelevant, false),
            (FeedKind::Hint, false),
            (FeedKind::System, true),
            (FeedKind::Truth, false),
        ] {
            assert_eq!(k.label().is_empty(), expect_empty);
        }
    }

    #[test]
    fn spinner_frames_are_stable() {
        let mut v = GameView { spinner_frame: 0, ..Default::default() };
        // 相邻帧不同（spinner 可见转动）。
        let f0 = v.spinner();
        v.spinner_frame = 1;
        assert_ne!(f0, v.spinner());
        // 索引远超帧数时按模循环。
        v.spinner_frame = 8;
        assert_eq!(f0, v.spinner());
    }

    #[test]
    fn editor_insert_backspace_handles_cjk() {
        let mut v = GameView::default();
        for c in "打嗝吗".chars() {
            v.insert_char(c);
        }
        assert_eq!(v.input, "打嗝吗");
        assert_eq!(v.input_cursor, 3);
        // Backspace 按字符删（不是按字节）。
        v.backspace();
        assert_eq!(v.input, "打嗝");
        assert_eq!(v.input_cursor, 2);
        // 光标移到中间插入。
        v.move_home();
        v.insert_char('在');
        assert_eq!(v.input, "在打嗝");
        assert_eq!(v.input_cursor, 1);
        // Delete 删光标处字符。
        v.delete();
        assert_eq!(v.input, "在嗝");
    }

    #[test]
    fn editor_cursor_moves_within_bounds() {
        let mut v = GameView::default();
        for c in "abc".chars() {
            v.insert_char(c);
        }
        v.move_left();
        v.move_left();
        v.move_left();
        v.move_left(); // 越界左移不 panic
        assert_eq!(v.input_cursor, 0);
        v.move_right();
        v.move_right();
        v.move_right();
        v.move_right(); // 越界右移不 panic
        assert_eq!(v.input_cursor, 3);
        v.clear_input();
        assert_eq!(v.input, "");
        assert_eq!(v.input_cursor, 0);
    }
}
