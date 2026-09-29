//! soup-cli：海龟汤终端摸鱼版（唯一入口）。
//! 子命令见 design-doc/02 §4；摸鱼件见 design-doc/06。
// by AI.Coding

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use turtle_soup::config::AppConfig;
use turtle_soup::dataset::{DatasetClient, FetchCursor, DEFAULT_ENDPOINT, MIRROR_ENDPOINT};
use turtle_soup::game::{GameService, MAX_QUESTIONS};
use turtle_soup::models::{HintLevel, Judgment, Puzzle, Session, SessionStatus};
use turtle_soup::session::{self, PuzzleStore};
use turtle_soup::{engine, logging, secrets};

/// 终端伪装标题（见 06 §1）。
const FAKE_TITLE: &str = "pnpm build";

type SharedSession = Arc<Mutex<Option<Session>>>;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        restore_title();
        eprintln!("\n[错误] {e}");
        if let Some(p) = logging::current_log_path() {
            eprintln!("       （详见日志：{}）", p.display());
        }
        std::process::exit(1);
    }
    restore_title();
}

async fn run() -> Result<(), turtle_soup::AppError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // 不带参数 = 随机一局（见 design-doc/02 §4.1）。
    let cmd = args.first().map(String::as_str).unwrap_or("play");
    let rest = args.get(1..).unwrap_or(&[]);
    match cmd {
        "play" => cmd_play(rest).await,
        "list" => cmd_list(),
        "ask" => cmd_ask(rest).await,
        "fetch" => cmd_fetch(rest).await,
        "config" => cmd_config(),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("未知命令：{other}\n");
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    println!(
        "海龟汤 · 终端摸鱼版\n\
         \n用法:\n\
         \x20 soup-cli                                  随机来一局（默认）\n\
         \x20 soup-cli list                              列出内置+已入库题目\n\
         \x20 soup-cli play [puzzle_id] [--difficulty N]  指定题 / 按难度随机\n\
         \x20 soup-cli play --resume <session_id>         继续未完成的对局\n\
         \x20 soup-cli ask <puzzle_id> <问题...>          单次判定（脚本/冒烟）\n\
         \x20 soup-cli fetch [--difficulty N] [--mirror]  拉取一批新题（100 条）\n\
         \x20 soup-cli config                            打印配置与日志路径\n\
         \n对局内: 直接输入即提问 | /guess <推理> | /hint | /hint2 | /status | /bb(伪装) | /quit\n\
         \nAPI Key: 环境变量 TURTLE_API_KEY 或 config.json 的 api_key。"
    );
}

// ---------------------------------------------------------------------------
// 子命令
// ---------------------------------------------------------------------------

fn cmd_list() -> Result<(), turtle_soup::AppError> {
    let store = PuzzleStore::load()?;
    if store.is_empty() {
        println!("题库为空。请先执行 `soup-cli fetch` 或检查 assets/puzzles.json。");
        return Ok(());
    }
    println!("共 {} 题：", store.len());
    for p in store.all() {
        println!(
            "  {:<14} D{}  {} 条事实  {}",
            p.id,
            p.difficulty,
            p.key_facts.len(),
            p.title
        );
    }
    Ok(())
}

fn cmd_config() -> Result<(), turtle_soup::AppError> {
    let path = AppConfig::resolved_path();
    println!("配置文件 : {}  {}", path.display(), if path.exists() { "（已找到）" } else { "（不存在，使用默认值）" });
    match AppConfig::load_or_default() {
        Ok(cfg) => {
            println!("base_url : {}", cfg.base_url);
            println!("model    : {}", cfg.model);
            println!("host_model: {}", cfg.host_model);
            match secrets::resolve(&cfg) {
                Ok(k) => println!("api_key  : {}（来源：env 或 config 内联）", secrets::mask(&k)),
                Err(_) => println!("api_key  : 未配置"),
            }
        }
        Err(e) => println!("配置加载失败: {e}"),
    }
    if let Some(p) = logging::current_log_path() {
        println!("日志文件 : {}", p.display());
    }
    Ok(())
}

async fn cmd_ask(args: &[String]) -> Result<(), turtle_soup::AppError> {
    let id = args.first().ok_or_else(|| missing("ask 需要 <puzzle_id>"))?;
    let question = args[1..].join(" ");
    if question.trim().is_empty() {
        return Err(missing("ask 需要 <问题>"));
    }
    let svc = build_service()?;
    let puzzle = svc.pick_puzzle(Some(id))?;
    let mut session = Session::new(format!("cli-{}", turtle_soup::models::now_ts()), &puzzle);
    let r = with_spinner("主持人思考中…", svc.ask_host(&puzzle, &mut session, &question)).await?;
    println!(
        "{}   (进度 {}/{})",
        verdict_line(r.judgment, &r.reply),
        r.progress.hit,
        r.progress.total
    );
    Ok(())
}

async fn cmd_fetch(args: &[String]) -> Result<(), turtle_soup::AppError> {
    let mut difficulty = 2u8;
    let mut endpoint = DEFAULT_ENDPOINT.to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--difficulty" | "-d" => {
                if let Some(d) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                    difficulty = d.clamp(1, 5);
                }
            }
            "--mirror" => endpoint = MIRROR_ENDPOINT.to_string(),
            _ => {}
        }
    }
    let client = DatasetClient::new(endpoint)?;
    let mut store = PuzzleStore::load()?;
    let mut cursor = FetchCursor::load()?;
    let outcome = with_spinner(
        "拉取并清洗中…",
        client.pull_into(&mut store, &mut cursor, difficulty),
    )
    .await?;
    println!(
        "拉取 {} 条，入库 {} 题{}。当前题库共 {} 题。",
        outcome.fetched,
        outcome.added,
        if outcome.done { "（已到末尾）" } else { "" },
        store.len()
    );
    Ok(())
}

async fn cmd_play(args: &[String]) -> Result<(), turtle_soup::AppError> {
    // 解析参数
    let mut puzzle_id: Option<String> = None;
    let mut resume_id: Option<String> = None;
    let mut difficulty: Option<u8> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--difficulty" | "-d" => difficulty = it.next().and_then(|s| s.parse::<u8>().ok()),
            "--resume" | "-r" => resume_id = it.next().cloned(),
            other if !other.starts_with('-') => puzzle_id = Some(other.to_string()),
            _ => {}
        }
    }

    let svc = build_service()?;

    // 恢复 or 新开
    let (puzzle, mut session) = if let Some(rid) = resume_id {
        let s = session::load_session(&rid)?;
        let p = svc.pick_puzzle(Some(&s.puzzle_id))?.clone();
        (p, s)
    } else {
        let p = match &puzzle_id {
            Some(id) => svc.pick_puzzle(Some(id))?,
            None => pick_random_puzzle(&svc, difficulty)?,
        };
        let s = Session::new(format!("cli-{}", turtle_soup::models::now_ts()), &p);
        (p, s)
    };
    if session.status == SessionStatus::Paused {
        session.resume(false);
    }
    session::save_session(&session)?;

    // 共享会话：Ctrl+C 时落盘并还原标题（见 06 §2、07 §8）
    let shared: SharedSession = Arc::new(Mutex::new(Some(session.clone())));
    spawn_interrupt_handler(shared.clone(), session.id.clone());

    if io::stdout().is_terminal() {
        set_title(FAKE_TITLE);
    }

    render_intro(&puzzle, &session);

    loop {
        print!("\n> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break; // EOF
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        // 伪装屏
        if input == "/bb" || input == "bb" {
            show_disguise();
            // 等一次回车再恢复
            let mut _skip = String::new();
            let _ = io::stdin().read_line(&mut _skip);
            redraw(&puzzle, &session);
            continue;
        }

        if let Some(rest) = input.strip_prefix("/guess ") {
            let r = with_spinner("裁判评定中…", svc.judge_guess(&puzzle, &mut session, rest)).await;
            handle(&shared, &mut session, r.map(|x| {
                format!(
                    "判定: {:?}  命中 {}/{}  点评: {}",
                    x.verdict,
                    x.hit_count,
                    x.hit_count + x.missed_count,
                    x.comment
                )
            }))?;
            if session.status.is_finished() {
                finish(&session, &puzzle);
                break;
            }
            continue;
        }

        match input {
            "/quit" => {
                session.pause();
                session::save_session(&session)?;
                *shared.lock().unwrap() = Some(session.clone());
                println!("已挂起并落盘（session {}）。下次：soup-cli play --resume {}", session.id, session.id);
                break;
            }
            "/status" => {
                println!(
                    "状态 {:?} | 已问 {} 问 | 命中 {:?} | 提示 {} 次 | 猜底失败 {} 次 | 剩余猜底 {} 次",
                    session.status,
                    session.question_count,
                    session.hit_facts,
                    session.hint_levels.len(),
                    session.guess_attempts_failed,
                    engine::remaining_guesses(session.guess_attempts_failed),
                );
                continue;
            }
            "/hint" => {
                let r = with_spinner("生成提示中…", svc.use_hint(&puzzle, &mut session, HintLevel::L1)).await;
                handle(&shared, &mut session, r.map(|x| format!("[L1 方向提示] {}", x.text)))?;
                continue;
            }
            "/hint2" => {
                let r = svc.use_hint(&puzzle, &mut session, HintLevel::L2).await;
                handle(&shared, &mut session, r.map(|x| format!("[L2 事实提示] {}", x.text)))?;
                continue;
            }
            _ => {}
        }

        if session.question_count >= MAX_QUESTIONS {
            println!("已达提问上限，请猜底或 /quit。");
            continue;
        }
        let r = with_spinner("主持人思考中…", svc.ask_host(&puzzle, &mut session, input)).await;
        handle(&shared, &mut session, r.map(|x| {
            format!(
                "{}   (进度 {}/{})",
                verdict_line(x.judgment, &x.reply),
                x.progress.hit,
                x.progress.total
            )
        }))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 渲染 / 摸鱼件
// ---------------------------------------------------------------------------

fn render_intro(puzzle: &Puzzle, session: &Session) {
    println!("\n=== 海龟汤 · 摸鱼版 ===");
    println!("题目: {}  (D{})", puzzle.title, puzzle.difficulty);
    println!("\n【汤面】{}\n", puzzle.surface);
    println!("直接输入即提问 | /guess <推理> | /hint | /hint2 | /status | /bb 伪装 | /quit");
    println!(
        "(提示上限 {} 次，猜底上限 {} 次)  已问 {} 问，进度 {}/{}\n",
        engine::max_hints(puzzle.fact_count()),
        engine::MAX_GUESS_ATTEMPTS,
        session.question_count,
        session.hit_facts.len(),
        puzzle.fact_count()
    );
}

/// 从伪装屏恢复：清屏后重绘对局（见 06 §2 必须重绘）。
fn redraw(puzzle: &Puzzle, session: &Session) {
    clear_screen();
    render_intro(puzzle, session);
    let shown: Vec<_> = session.messages.iter().rev().take(6).collect();
    if !shown.is_empty() {
        println!("--- 最近对话 ---");
        for m in shown.into_iter().rev() {
            let who = match m.role {
                turtle_soup::models::Role::Player => "你",
                turtle_soup::models::Role::Host => "主持人",
            };
            println!("{who}: {}", m.text);
        }
    }
}

/// 伪装屏：清屏 + 打印"假构建日志"（见 06 §3）。
fn show_disguise() {
    if !io::stdout().is_terminal() {
        println!("$ pnpm build\n✓ built in 1.92s");
        return;
    }
    clear_screen();
    let logs: [&[&str]; 3] = [
        &[
            "$ pnpm build",
            "▲ vite v5.4.21 building for production...",
            "✓ 41 modules transformed.",
            "dist/index.html                 0.39 kB",
            "dist/assets/index-D4f2a1.js    86.08 kB │ gzip: 34.12 kB",
            "✓ built in 1.92s",
        ],
        &[
            "$ cargo test --workspace",
            "   Compiling turtle-soup v0.1.0",
            "    Finished test [unoptimized] target(s) in 2.04s",
            "     Running unittests src/lib.rs",
            "test result: ok. 42 passed; 0 failed",
        ],
        &[
            "$ pnpm install",
            "Progress: resolved 214, reused 210, downloaded 0",
            "Packages: +214",
            "Done in 6.1s using pnpm v10.34.6",
        ],
    ];
    let idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as usize)
        % logs.len();
    for l in logs[idx] {
        println!("{l}");
    }
    print!("\n（按回车返回）");
    io::stdout().flush().ok();
}

/// 终端标题伪装（ANSI OSC）。
fn set_title(t: &str) {
    if io::stdout().is_terminal() {
        print!("\x1b]0;{t}\x07");
        io::stdout().flush().ok();
    }
}

fn restore_title() {
    if io::stdout().is_terminal() {
        print!("\x1b]0; \x07");
        io::stdout().flush().ok();
    }
}

fn clear_screen() {
    if io::stdout().is_terminal() {
        print!("\x1b[2J\x1b[H");
        io::stdout().flush().ok();
    }
}

/// 捕获 Ctrl+C：落盘 + 还原标题 + 退出（见 07 §8）。
fn spawn_interrupt_handler(shared: SharedSession, session_id: String) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            if let Some(s) = shared.lock().unwrap().clone() {
                let _ = session::save_session(&s);
            }
            restore_title();
            eprintln!("\n\n已保存对局并退出。下次：soup-cli play --resume {session_id}");
            std::process::exit(0);
        }
    });
}

fn handle<T: std::fmt::Display>(
    shared: &SharedSession,
    session: &mut Session,
    r: Result<T, turtle_soup::AppError>,
) -> Result<(), turtle_soup::AppError> {
    match r {
        Ok(msg) => {
            println!("{msg}");
            session::save_session(session)?;
            *shared.lock().unwrap() = Some(session.clone());
            Ok(())
        }
        Err(e) => {
            // 不静默降级：明确报错（见 07 §4），并给出日志位置。
            eprintln!("[异常] {e}");
            if let Some(p) = logging::current_log_path() {
                eprintln!("       （详见日志：{}）", p.display());
            }
            Ok(())
        }
    }
}

fn finish(session: &Session, puzzle: &Puzzle) {
    println!("\n=== 本局结束：{} ===", status_cn(session.status));
    println!("【汤底】{}", puzzle.truth);
    if session.status == SessionStatus::Won {
        let s = engine::score(
            puzzle.difficulty,
            session.question_count,
            session.hints_used,
            session.guess_attempts_failed,
        );
        println!("得分 {}  星级 {}", s.score, "★".repeat(s.stars as usize));
    }
}

/// 合并判定标签与主持人回复，避免"【否】不是。"这类重复：
/// 当回复只是判定词的复述（或为空）时只显示标签。
fn verdict_line(j: Judgment, reply: &str) -> String {
    let label = judgment_cn(j);
    let r = reply.trim().trim_end_matches(['。', '.', '！', '!', ' ', '~']);
    let pure_verdict = matches!(
        r,
        "是" | "是的" | "对" | "对的" | "没错" | "正确"
            | "不是" | "不" | "不对" | "否" | "错误"
            | "无关" | "对了一半" | "一半" | "部分对"
    );
    if r.is_empty() || pure_verdict {
        label.to_string()
    } else {
        format!("{label} {reply}")
    }
}

fn judgment_cn(j: Judgment) -> &'static str {
    use Judgment::*;
    match j {
        Yes => "【是】",
        No => "【否】",
        Irrelevant => "【无关】",
        Partial => "【一半】",
    }
}

fn status_cn(s: SessionStatus) -> &'static str {
    use SessionStatus::*;
    match s {
        Won => "猜中",
        Lost => "猜底次数耗尽",
        Abandoned => "弃局",
        Paused => "挂起",
        _ => "进行中",
    }
}

fn build_service() -> Result<GameService, turtle_soup::AppError> {
    let cfg = AppConfig::load_or_default()?;
    cfg.validate()?;
    let key = secrets::resolve(&cfg)?;
    GameService::new(&cfg, key)
}

/// 随机选题：优先未通关的题，其次任意（见 design-doc/01 §6.2）。
fn pick_random_puzzle(svc: &GameService, difficulty: Option<u8>) -> Result<Puzzle, turtle_soup::AppError> {
    use std::collections::HashSet;
    let cands = svc.store.candidates(difficulty);
    if cands.is_empty() {
        return Err(missing("没有可用题目"));
    }
    let finished: HashSet<String> = session::list_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.status.is_finished())
        .map(|s| s.puzzle_id)
        .collect();
    let unplayed: Vec<&Puzzle> = cands.iter().copied().filter(|p| !finished.contains(&p.id)).collect();
    let pool = if unplayed.is_empty() { cands } else { unplayed };
    Ok(pool[rand_index(pool.len())].clone())
}

/// 基于时间的伪随机下标（不引入 rand 依赖；用位混合避免线性规律）。
fn rand_index(n: usize) -> usize {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x as usize) % n.max(1)
}

fn missing(msg: &str) -> turtle_soup::AppError {
    turtle_soup::AppError::new(turtle_soup::ErrorCode::InvalidState, msg)
}

/// 包一层终端转圈提示，缓解"回车后以为卡住"。
/// 仅当 stdout 是真实终端时显示；输出被重定向（管道/文件）时自动跳过。
async fn with_spinner<T, F>(label: &str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    if !io::stdout().is_terminal() {
        return fut.await;
    }
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let text = label.to_string();
    let text_for_task = text.clone();
    let spinner = tokio::spawn(async move {
        let frames = ['|', '/', '-', '\\'];
        // 先等约 150ms，极快响应就不闪烁了。
        for _ in 0..2 {
            if flag.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        let mut i = 0usize;
        while !flag.load(Ordering::Relaxed) {
            print!("\r  {} {}", frames[i % frames.len()], text_for_task);
            io::stdout().flush().ok();
            i += 1;
            tokio::time::sleep(Duration::from_millis(90)).await;
        }
    });

    let out = fut.await;
    done.store(true, Ordering::Relaxed);
    let _ = spinner.await;
    // 清掉转圈那一行，让结果从行首打印。
    print!("\r{}\r", " ".repeat(text.chars().count() + 8));
    io::stdout().flush().ok();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_line_collapses_pure_verdict() {
        assert_eq!(verdict_line(Judgment::No, "不是。"), "【否】");
        assert_eq!(verdict_line(Judgment::Yes, "是"), "【是】");
        assert_eq!(verdict_line(Judgment::Yes, "是。"), "【是】");
        assert_eq!(verdict_line(Judgment::Irrelevant, ""), "【无关】");
        assert_eq!(verdict_line(Judgment::Partial, "对了一半"), "【一半】");
    }

    #[test]
    fn verdict_line_keeps_extra_info() {
        assert_eq!(
            verdict_line(Judgment::Partial, "对了一半，动机不对"),
            "【一半】 对了一半，动机不对"
        );
        assert_eq!(verdict_line(Judgment::No, "他并不想自杀"), "【否】 他并不想自杀");
    }

    #[test]
    fn rand_index_in_range() {
        for n in 1..20 {
            assert!(rand_index(n) < n);
        }
        assert_eq!(rand_index(0), 0);
    }
}
