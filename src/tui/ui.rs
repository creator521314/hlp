//! ratatui 卡片渲染（v0.4 设计 §4.3）。
//!
//! 布局：命令区 / 逐段解释区 / 输出窗格 / 快捷键行。
//! 终端高度 < 10 行时退化为纯命令行渲染（只画命令 + 快捷键行）。

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, State};

pub const SHORTCUTS: &str = "[D]ry-run [C]opy [E]xecute [R]efine [A]lternative [X]解释 [Q]uit";

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.height < 10 {
        render_compact(f, app);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(4),
            Constraint::Length(if app.output.is_empty() { 0 } else { 6 }),
            Constraint::Length(2),
        ])
        .split(area);

    // ① 命令区：当前选中候选 + 来源分组
    let current = app.current_command();
    let command = if app.state == State::Generating && !app.preview.is_empty() {
        app.preview.as_str()
    } else {
        current
    };
    let group = app
        .candidates
        .get(app.selected)
        .map(|c| c.group.label())
        .unwrap_or("");
    let title = format!(
        " hlp v0.4  {group}  {}/{} ",
        app.selected + 1,
        app.candidates.len().max(1)
    );
    let cmd_lines = vec![
        Line::from(Span::styled(
            command.to_string(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("意图：{}", app.intent),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::raw(app.status.clone())),
    ];
    f.render_widget(
        Paragraph::new(cmd_lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true }),
        chunks[0],
    );

    // ② 逐段解释区（或输入提示）
    let mut lines: Vec<Line> = Vec::new();
    match app.state {
        State::Input => lines.push(Line::from(format!("输入意图：{}_", app.input))),
        State::Refine => lines.push(Line::from(format!("修正要求：{}_", app.input))),
        State::ConfirmFatal => {
            lines.push(Line::from("致命命令：请完整重新输入命令原文：".to_string()));
            lines.push(Line::from(format!("{}_", app.input)));
        }
        State::ConfirmExec | State::ConfirmWarning => {
            lines.push(Line::from(format!("确认执行? [y/N]：{}_", app.input)))
        }
        _ => {
            if app.explanation.is_empty() {
                lines.push(Line::from(Span::styled(
                    "（暂无逐段解释，按 X 重新获取）",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                let width = app
                    .explanation
                    .iter()
                    .map(|s| s.cmd.chars().count())
                    .max()
                    .unwrap_or(0);
                for seg in &app.explanation {
                    let note = if seg.note.is_empty() {
                        "（无本地注解）"
                    } else {
                        seg.note.as_str()
                    };
                    let pad = width.saturating_sub(seg.cmd.chars().count());
                    lines.push(Line::from(format!(
                        "  {}{}  {}",
                        seg.cmd,
                        " ".repeat(pad),
                        note
                    )));
                }
            }
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" 逐段解释 "))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );

    // ③ 输出窗格（dry-run 预览 / 执行结果）
    if !app.output.is_empty() && chunks[2].height > 0 {
        let out = Paragraph::new(app.output.clone())
            .block(Block::default().borders(Borders::ALL).title(" 输出 "))
            .wrap(Wrap { trim: false });
        f.render_widget(out, chunks[2]);
    }

    // ④ 快捷键行
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            SHORTCUTS.to_string(),
            Style::default().fg(Color::Yellow),
        ))),
        chunks[3],
    );
}

/// 终端过矮时的退化渲染：只画命令 + 快捷键行。
fn render_compact(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(
        Paragraph::new(format!("{}  {}", app.current_command(), app.status)),
        chunks[0],
    );
    f.render_widget(Paragraph::new(SHORTCUTS), chunks[1]);
}
