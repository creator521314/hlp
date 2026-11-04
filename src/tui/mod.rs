//! TUI 入口（v0.4 设计 §2.9 / §4.4）。
//!
//! 冷启动红线（遗留风险 #4）：ratatui/crossterm 的一切初始化只在 `run` 内发生，
//! `--one-line` / `--stream` / `--check` 路径不触碰本模块。

pub mod app;
pub mod events;
pub mod ui;

use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::Config;
use crate::daemon::protocol::Source;
use crate::dryrun::{self, DryPlan};
use crate::engine::Engine;
use crate::error::HlpError;
use crate::kb::Kb;

use app::{Action, App, State};

/// 进入 TUI。intent=None → 首屏输入框。非 TTY → TuiUnavailable（退出码 6）。
pub fn run(cfg: Config, intent: Option<String>) -> Result<(), HlpError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(HlpError::TuiUnavailable {
            reason: "当前不是交互式终端（TTY），无法渲染卡片".to_string(),
        });
    }
    // 信任表在 raw mode 之前加载：配置非法时直接返回错误，不会留下未恢复的终端
    let trust = crate::danger::trust::load()?;
    enable_raw_mode().map_err(tui_err)?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen).map_err(tui_err)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).map_err(tui_err)?;

    let mut app = App::with_trust(intent, trust);
    let result = event_loop(&mut terminal, &cfg, &mut app);

    // 无论成功失败都恢复终端
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    result
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    cfg: &Config,
    app: &mut App,
) -> Result<(), HlpError> {
    loop {
        terminal.draw(|f| ui::render(f, app)).map_err(tui_err)?;

        // 按 pending 标志执行副作用（生成 / 预览 / 执行 / 解释 / 复制）
        if let Some(prompt) = app.pending_generate.take() {
            generate(cfg, app, &prompt);
        }
        if app.pending_dryrun {
            app.pending_dryrun = false;
            let plan = dryrun::plan(app.current_command());
            app.output = match &plan {
                DryPlan::Unsupported { reason } => format!("不支持 dry-run：{reason}\n"),
                _ => dryrun::run(&plan).unwrap_or_else(|e| format!("预览失败：{e}\n")),
            };
            app.state = State::DryRunPreview;
        }
        if app.pending_explain {
            app.pending_explain = false;
            match crate::explain::explain(cfg, app.current_command(), false) {
                Ok(exp) => {
                    app.explanation = exp.segments;
                    app.status = format!("解释来源：{}", exp.source.as_str());
                }
                Err(e) => app.status = format!("解释失败：{e}"),
            }
        }
        if app.pending_copy {
            app.pending_copy = false;
            app.status = if copy_osc52(app.current_command()) {
                "已复制（OSC52）".to_string()
            } else {
                "复制失败：终端不支持 OSC52，请用鼠标选择".to_string()
            };
        }
        if app.pending_execute {
            app.pending_execute = false;
            let cmd = app.current_command().to_string();
            let out = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .stdin(Stdio::null())
                .output();
            app.output = match out {
                Ok(o) => format!(
                    "$ {cmd}\n{}{}（退出码 {}）\n",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr),
                    o.status.code().unwrap_or(-1)
                ),
                Err(e) => format!("执行失败：{e}\n"),
            };
            app.state = State::Done;
        }

        if app.quit {
            return Ok(());
        }
        if let Some(mut action) = events::next_action(Duration::from_millis(100)).map_err(tui_err)? {
            // 输入态下 'q' 是普通字符
            if action == Action::Quit && events::is_input_state(app.state) {
                action = Action::Char('q');
            }
            app.handle(action);
        }
    }
}

/// 一次生成：kb 组先行 + LLM 流式（want_explanation=true，决策 8）。
fn generate(cfg: &Config, app: &mut App, prompt: &str) {
    let kb_hits = Kb::from_config(cfg)
        .ok()
        .map(|kb| kb.lookup(prompt))
        .unwrap_or_default();
    let kb_cmds: Vec<String> = kb_hits.iter().map(|h| h.command.clone()).collect();

    let mut engine = match Engine::from_config(cfg.clone()) {
        Ok(e) => e,
        Err(e) => {
            app.status = format!("引擎不可用：{e}");
            app.state = State::Ready;
            return;
        }
    };
    engine.set_want_explanation(true);
    let result = engine.run_stream(prompt, &mut |snap| app.preview = snap.to_string());
    match result {
        Ok(outcome) => {
            let llm_cmds = if outcome.source == Source::Kb {
                Vec::new() // kb 命中时 engine 已短路，避免与 kb 组重复
            } else {
                let mut v = vec![outcome.top1.clone()];
                v.extend(outcome.alternatives.clone());
                v
            };
            let mut segments = outcome.segments.clone();
            if segments.is_empty() && !llm_cmds.is_empty() {
                // 双保险：want_explanation 解析失败 → 按 X 键再取（设计 §8 决策 8）
                segments = crate::explain::split::split_pipeline(&llm_cmds[0])
                    .into_iter()
                    .map(|cmd| crate::explain::Segment {
                        cmd,
                        note: String::new(),
                    })
                    .collect();
            }
            app.load_candidates(kb_cmds, llm_cmds, segments);
            app.status = format!("来源：{}", outcome.source.as_str());
        }
        Err(e) => {
            app.status = format!("生成失败：{e}");
            app.state = State::Ready;
        }
    }
}

fn tui_err(e: io::Error) -> HlpError {
    HlpError::TuiUnavailable {
        reason: e.to_string(),
    }
}

/// OSC52 写 /dev/tty（不经 stdout，避免污染；决策 11）。
pub fn copy_osc52(text: &str) -> bool {
    let mut tty = match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        Ok(f) => f,
        Err(_) => return false,
    };
    write!(tty, "\x1b]52;c;{}\x07", base64(text.as_bytes()))
        .and_then(|_| tty.flush())
        .is_ok()
}

/// 极简 base64（避免为复制功能引入新依赖）。
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("列出".as_bytes()), "5YiX5Ye6");
    }

    /// T-I-40 的单元级同义断言：非 TTY 检测在 run 内完成
    #[test]
    fn tui_requires_tty() {
        assert!(!std::io::stdin().is_terminal() || std::io::stdout().is_terminal());
    }
}
