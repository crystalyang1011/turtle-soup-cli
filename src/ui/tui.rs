//! 全屏 TUI 会话：alternate screen 进出（含 panic 还原钩子）+ 事件循环。
//! 见 docs/2-tui/01-界面设计.md §1.3、§5。
//!
//! 本文件只做**渲染与交互编排**：判定仍走 `game.rs`（svc），会话落盘仍走 `session.rs`。
// by AI.Coding

use crate::models::{AppError, ErrorCode};
use crate::ui::{panels, GameView};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::future::Future;
use std::io::{stdout, Stdout};
use std::time::Duration;

/// 全屏会话句柄：Drop 时兜底还原终端（正常路径显式 `restore`）。
pub struct TuiSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiSession {
    /// 进入 alternate screen + raw mode，并挂 panic 还原钩子（01-界面设计.md §7.5）。
    pub fn enter() -> Result<Self, AppError> {
        enable_raw_mode().map_err(io_err)?;
        let mut out = stdout();
        crossterm::execute!(out, EnterAlternateScreen).map_err(io_err)?;
        install_panic_hook();
        let backend = CrosstermBackend::new(out);
        let terminal = Terminal::new(backend).map_err(io_err)?;
        Ok(Self { terminal })
    }

    /// 终端句柄（装配层绘制用）。
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// 还原终端状态（幂等：重复调用无害）。
    pub fn restore(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(stdout(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        self.restore();
    }
}

fn io_err(e: std::io::Error) -> AppError {
    AppError::new(ErrorCode::Io, format!("终端初始化失败: {e}"))
}

/// panic 时也要还原终端：替换默认 hook，先还原再按原逻辑 panic。
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}

/// 是否满足全屏 TUI 最低尺寸（01-界面设计.md §1.1：80×24）。
pub fn size_ok() -> bool {
    match crossterm::terminal::size() {
        Ok((w, h)) => w >= 80 && h >= 24,
        Err(_) => false,
    }
}

/// 长度截断（消息流容量控制用，纯函数供测试）。
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// T1.0 事件循环占位说明：真正的对局循环在 `bin/soup_cli.rs::run_tui_game` 装配
/// （crossterm EventStream 与 LLM future 经 `tokio::select!` 并发，100ms 重绘驱动 spinner）。
/// 本模块提供键盘语义解析的纯函数，供装配层与测试复用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// 提交输入。
    Submit,
    /// 清空输入缓冲（Esc）。
    ClearInput,
    /// 退出（落盘由装配层负责）。
    Quit,
    /// 删除光标前一个字符（Backspace）。
    Backspace,
    /// 删除光标处字符（Delete）。
    Delete,
    /// 光标左移。
    MoveLeft,
    /// 光标右移。
    MoveRight,
    /// 光标移到行首（Home / Ctrl+A）。
    Home,
    /// 光标移到行尾（End / Ctrl+E）。
    End,
    /// 可打印字符（含中文），插入到光标处。
    Input(char),
    /// 忽略的按键。
    Ignore,
}

/// 键位语义（01-界面设计.md §5）：回车提交、Esc 清空、Ctrl+C 退出、编辑键单行编辑、其余字符入缓冲。
pub fn map_key(key: KeyEvent) -> KeyAction {
    // Windows 终端会发 Press/Release 两种事件，只响应 Press 避免重复。
    if key.kind != KeyEventKind::Press {
        return KeyAction::Ignore;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => KeyAction::Submit,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Delete => KeyAction::Delete,
        KeyCode::Left => KeyAction::MoveLeft,
        KeyCode::Right => KeyAction::MoveRight,
        KeyCode::Home => KeyAction::Home,
        KeyCode::End => KeyAction::End,
        KeyCode::Esc => KeyAction::ClearInput,
        KeyCode::Char('c') if ctrl => KeyAction::Quit,
        KeyCode::Char('a') if ctrl => KeyAction::Home,
        KeyCode::Char('e') if ctrl => KeyAction::End,
        KeyCode::Char(c) if !ctrl => KeyAction::Input(c),
        _ => KeyAction::Ignore,
    }
}

/// 绘制一帧（装配层每 100ms / 每次事件后调用）。
pub fn draw(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    v: &GameView,
    scroll_offset: u16,
) -> Result<(), AppError> {
    terminal
        .draw(|f| panels::render_all(f, v, scroll_offset))
        .map_err(io_err)?;
    Ok(())
}

/// 等待一次按键。`timeout=None` 表示无限阻塞（**空闲时不重绘，光标按终端自然频率闪烁**）；
/// `Some(d)` 用于 busy 态的 100ms 心跳。
///
/// 返回 `Ok(None)` = 超时/非按键事件；`Ok(Some(action))` = 一次按键；
/// 事件流结束（stdin 关闭）时返回 `Ok(Some(Quit))` 以优雅退出。
pub async fn poll_key(
    events: &mut EventStream,
    timeout: Option<Duration>,
) -> Result<Option<KeyAction>, AppError> {
    let ev = match timeout {
        Some(d) => match tokio::time::timeout(d, events.next()).await {
            Ok(ev) => ev,
            Err(_) => return Ok(None), // 超时：交回外层（busy 时推进 spinner）
        },
        None => events.next().await,
    };
    match ev {
        Some(Ok(Event::Key(k))) => Ok(Some(map_key(k))),
        Some(Ok(_)) => Ok(None),
        Some(Err(e)) => Err(AppError::new(ErrorCode::Io, format!("终端事件读取失败: {e}"))),
        None => Ok(Some(KeyAction::Quit)),
    }
}

/// 等待一个 future（通常是一次 LLM 调用）完成，**期间每 100ms 重绘一帧驱动 spinner 动画**。
/// 前提：传入的 future **不得借用 `view`/`terminal`**（只借用 session 等对局状态），
/// 否则与这里的 `&mut view` 冲突——这是刻意的分层约束。
pub async fn run_animated<F: Future>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    view: &mut GameView,
    fut: F,
) -> F::Output {
    let _ = draw(terminal, view, 0); // 立刻显示 busy 态，避免首个 100ms 白屏
    tokio::pin!(fut);
    loop {
        tokio::select! {
            out = &mut fut => return out,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                view.spinner_frame = view.spinner_frame.wrapping_add(1);
                let _ = draw(terminal, view, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn key_mapping_covers_contract() {
        assert_eq!(map_key(key(KeyCode::Enter, KeyModifiers::NONE)), KeyAction::Submit);
        assert_eq!(map_key(key(KeyCode::Esc, KeyModifiers::NONE)), KeyAction::ClearInput);
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            KeyAction::Quit
        );
        // 编辑键（Backspace/Delete/方向/Home/End）。
        assert_eq!(map_key(key(KeyCode::Backspace, KeyModifiers::NONE)), KeyAction::Backspace);
        assert_eq!(map_key(key(KeyCode::Delete, KeyModifiers::NONE)), KeyAction::Delete);
        assert_eq!(map_key(key(KeyCode::Left, KeyModifiers::NONE)), KeyAction::MoveLeft);
        assert_eq!(map_key(key(KeyCode::Right, KeyModifiers::NONE)), KeyAction::MoveRight);
        assert_eq!(map_key(key(KeyCode::Home, KeyModifiers::NONE)), KeyAction::Home);
        assert_eq!(map_key(key(KeyCode::End, KeyModifiers::NONE)), KeyAction::End);
        assert_eq!(map_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL)), KeyAction::Home);
        assert_eq!(map_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL)), KeyAction::End);
        // 小写 c 不带 CTRL 是普通输入。
        assert_eq!(map_key(key(KeyCode::Char('c'), KeyModifiers::NONE)), KeyAction::Input('c'));
        assert_eq!(map_key(key(KeyCode::Char('好'), KeyModifiers::NONE)), KeyAction::Input('好'));
        // 其它 Ctrl 组合与功能键忽略。
        assert_eq!(map_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), KeyAction::Ignore);
        assert_eq!(map_key(key(KeyCode::F(5), KeyModifiers::NONE)), KeyAction::Ignore);
    }

    #[test]
    fn release_events_are_ignored() {
        let mut k = key(KeyCode::Enter, KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(map_key(k), KeyAction::Ignore);
    }

    #[test]
    fn truncate_chars_limits_and_keeps_cjk() {
        assert_eq!(truncate_chars("你好世界", 2), "你好");
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("", 3), "");
    }
}
