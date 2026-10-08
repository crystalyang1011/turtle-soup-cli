//! soup-cli：海龟汤终端摸鱼版（唯一入口）。
//! 子命令见 docs/1-turtle-cli/02 §4；摸鱼件见 docs/1-turtle-cli/06。
// by AI.Coding

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use turtle_soup::config::AppConfig;
use turtle_soup::dataset::{DatasetClient, FetchCursor, DEFAULT_ENDPOINT, MIRROR_ENDPOINT};
use turtle_soup::game::{GameService, MAX_QUESTIONS};
use turtle_soup::models::{ErrorCode, Judgment, Puzzle, Session, SessionStatus};
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
    // 不带参数 = 随机一局（见 docs/1-turtle-cli/02 §4.1）。
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
         \x20 soup-cli fetch [--difficulty N] [--mirror]  拉取并清洗 TurtleBench 中文题源\n\
         \x20 soup-cli config                            打印配置与日志路径\n\
         \n对局内: 直接输入即提问 | /guess <推理> | /hint | /answer | /switch [id] | /hide | /list | /status | /quit\n\
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
    let hidden = session::load_hidden();
    for p in store.all() {
        let mark = if hidden.contains(&p.id) { "  （已隐藏）" } else { "" };
        println!(
            "  {:<14} D{}  {} 条事实  {}{}",
            p.id,
            p.difficulty,
            p.key_facts.len(),
            p.title,
            mark
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
        "题源共 {} 个故事，本次新增入库 {} 题。当前题库共 {} 题。",
        outcome.fetched,
        outcome.added,
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
    let (mut puzzle, mut session) = if let Some(rid) = resume_id {
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
    spawn_interrupt_handler(shared.clone());

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
                    "状态 {:?} | 已问 {} 问 | 命中 {:?} | 提示 {} 次 | 猜底失败 {} 次",
                    session.status,
                    session.question_count,
                    session.hit_facts,
                    session.hint_levels.len(),
                    session.guess_attempts_failed,
                );
                continue;
            }
            "/list" => {
                let hidden = session::load_hidden();
                for p in svc.store.all() {
                    let mark = if hidden.contains(&p.id) { "  （已隐藏）" } else { "" };
                    println!("  {:<14} D{}  {} 条事实  {}{}", p.id, p.difficulty, p.key_facts.len(), p.title, mark);
                }
                continue;
            }
            "/hide" => {
                // 标记**当前**题目"不再显示"：先让用户确认，确认后才隐藏并换题（见 03 §4）。
                let target = session.puzzle_id.clone();
                print!("确认隐藏当前题目「{}」并换一题？[y/N] ", puzzle.title);
                io::stdout().flush().ok();
                let mut ans = String::new();
                let ans = if io::stdin().read_line(&mut ans)? == 0 {
                    String::new()
                } else {
                    ans.trim().to_ascii_lowercase()
                };
                if ans != "y" && ans != "yes" {
                    println!("已取消。");
                    continue;
                }
                session::hide_puzzle(&target)?;
                println!("已标记隐藏：{target}，随机选题不再显示（用 /unhide {target} 恢复）。");
                // 当前题不再显示 → 自动换一题（无可见题则停在原地）。
                session.pause();
                session::save_session(&session)?;
                match pick_random_puzzle(&svc, None) {
                    Ok(new_puzzle) => {
                        let new_session = Session::new(
                            format!("cli-{}", turtle_soup::models::now_ts()),
                            &new_puzzle,
                        );
                        session::save_session(&new_session)?;
                        *shared.lock().unwrap() = Some(new_session.clone());
                        puzzle = new_puzzle;
                        session = new_session;
                        render_intro(&puzzle, &session);
                    }
                    Err(e) => println!("{}", e.message),
                }
                continue;
            }
            "/unhide" => {
                let arg = input.strip_prefix("/unhide").unwrap_or("").trim();
                if arg.is_empty() {
                    println!("用法：/unhide <puzzle_id>（用 /list 查看）");
                    continue;
                }
                match session::unhide_puzzle(arg)? {
                    true => println!("已取消隐藏：{arg}"),
                    false => println!("该题未被隐藏：{arg}"),
                }
                continue;
            }
            "/switch" | "/pick" => {
                // 切换题目：/switch <puzzle_id> 指定，/switch 随机换一题。
                // 当前对局挂起落盘后开新局（见 02 §4.2）。
                let target = input
                    .strip_prefix("/switch")
                    .or_else(|| input.strip_prefix("/pick"))
                    .map(str::trim)
                    .unwrap_or("");
                let new_puzzle = if target.is_empty() {
                    match pick_random_puzzle(&svc, None) {
                        Ok(p) => p,
                        Err(e) => {
                            println!("{}", e.message);
                            continue;
                        }
                    }
                } else {
                    match svc.store.get(target) {
                        Some(p) => p.clone(),
                        None => {
                            println!("题库中不存在题目 {target}（用 /list 查看可选 id）");
                            continue;
                        }
                    }
                };
                // 挂起旧局并落盘，再开新局。
                session.pause();
                session::save_session(&session)?;
                let new_session = Session::new(
                    format!("cli-{}", turtle_soup::models::now_ts()),
                    &new_puzzle,
                );
                session::save_session(&new_session)?;
                *shared.lock().unwrap() = Some(new_session.clone());
                puzzle = new_puzzle;
                session = new_session;
                render_intro(&puzzle, &session);
                continue;
            }
            "/hint" => {
                // 只给方向提示，不泄底、不分级、不带上限（见 01 §5）。
                let r = with_spinner("生成提示中…", svc.use_hint(&puzzle, &mut session)).await;
                handle(&shared, &mut session, r.map(|x| x.text))?;
                continue;
            }
            "/answer" => {
                // 玩家主动查看汤底：**必须二次确认**；确认即弃局结算，结算后才展示汤底
                // （见 02 §4.2、07 §2 反泄底例外）。
                print!("确认放弃本局、查看「{}」的汤底？[y/N] ", puzzle.title);
                io::stdout().flush().ok();
                let mut ans = String::new();
                let ans = if io::stdin().read_line(&mut ans)? == 0 {
                    String::new()
                } else {
                    ans.trim().to_ascii_lowercase()
                };
                if ans != "y" && ans != "yes" {
                    println!("已取消。");
                    continue;
                }
                session.abandon();
                session::save_session(&session)?;
                *shared.lock().unwrap() = Some(session.clone());
                finish(&session, &puzzle);
                break;
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
    println!("直接输入即提问 | /guess <推理> | /hint | /answer | /switch [id] | /hide | /list | /status | /quit");
    println!(
        "(提示、猜底均不限次)  已问 {} 问，进度 {}/{}\n",
        session.question_count,
        session.hit_facts.len(),
        puzzle.fact_count()
    );
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

/// 捕获 Ctrl+C：落盘 + 还原标题 + 退出（见 07 §8）。
/// 会话 id 从 shared 实时取，兼容 `/switch` 中途换局。
fn spawn_interrupt_handler(shared: SharedSession) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let id = if let Some(s) = shared.lock().unwrap().clone() {
                let _ = session::save_session(&s);
                s.id
            } else {
                String::from("(未知)")
            };
            restore_title();
            eprintln!("\n\n已保存对局并退出。下次：soup-cli play --resume {id}");
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
            // 对局内的预期状态（已达上限 / 本局已结束）按普通提示渲染后继续（见 02 §4.3）。
            if e.code == ErrorCode::InvalidState {
                println!("{}", e.message);
                return Ok(());
            }
            // 判定解析失败（重调后仍未拿到 JSON）：友好提示 + 日志位置，不标 `[异常]`（见 07 §4）。
            if e.code == ErrorCode::ParseFailed {
                println!("判定失败，请重试。");
                if let Some(p) = logging::current_log_path() {
                    println!("       （详见日志：{}）", p.display());
                }
                return Ok(());
            }
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

/// 随机选题：先剔除已隐藏的题，再优先未通关的题，其次任意（见 01 §6.2、03 §4）。
fn pick_random_puzzle(svc: &GameService, difficulty: Option<u8>) -> Result<Puzzle, turtle_soup::AppError> {
    use std::collections::HashSet;
    let hidden = session::load_hidden();
    let cands: Vec<&Puzzle> = svc
        .store
        .candidates(difficulty)
        .into_iter()
        .filter(|p| !hidden.contains(&p.id))
        .collect();
    if cands.is_empty() {
        return Err(missing("没有可用题目（可能都被隐藏了，用 /unhide <id> 恢复）"));
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
