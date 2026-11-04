//! 按键 → Action 映射（v0.4 设计 §2.9 快捷键行）。

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

use super::app::Action;

/// 阻塞最多 `timeout` 等一个按键；非按键事件忽略。
pub fn next_action(timeout: Duration) -> io::Result<Option<Action>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let Event::Key(key) = event::read()? else {
        return Ok(None);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(None);
    }
    Ok(Some(match key.code {
        KeyCode::Esc => Action::Esc,
        KeyCode::Enter => Action::Enter,
        KeyCode::Backspace => Action::Backspace,
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('a') => Action::Alternative,
        KeyCode::Char('r') => Action::Refine,
        KeyCode::Char('d') => Action::DryRun,
        KeyCode::Char('x') => Action::Explain,
        KeyCode::Char('c') => Action::Copy,
        KeyCode::Char('e') => Action::Execute,
        KeyCode::Char(c) => Action::Char(c),
        _ => return Ok(None),
    }))
}

/// 输入态下 'q' 是普通字符，不应退出（否则没法输入含 q 的意图）。
pub fn is_input_state(state: super::app::State) -> bool {
    use super::app::State;
    matches!(
        state,
        State::Input | State::Refine | State::ConfirmFatal | State::ConfirmExec | State::ConfirmWarning
    )
}
