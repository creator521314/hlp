//! TUI 状态机（v0.4 设计 §2.9）——只管状态与动作，不含渲染、不含 IO。
//!
//! `[E]` 是 v0.4 唯一的命令执行路径，必须先过 danger 熔断门；
//! 状态机本身不执行命令（由 mod.rs 的事件循环按 pending_* 标志执行）。

use crate::danger::{self, trust::TrustList};
use crate::explain::Segment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Input,
    Generating,
    Ready,
    Refine,
    DryRunPreview,
    ConfirmExec,
    ConfirmWarning,
    ConfirmFatal,
    Executing,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Kb,
    Llm,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Kb => "[知识库]",
            Group::Llm => "[LLM]",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateItem {
    pub command: String,
    pub group: Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Enter,
    Esc,
    Char(char),
    Backspace,
    Alternative,
    Refine,
    DryRun,
    Explain,
    Copy,
    Execute,
}

pub struct App {
    pub state: State,
    pub intent: String,
    pub input: String,
    pub candidates: Vec<CandidateItem>,
    pub selected: usize,
    pub explanation: Vec<Segment>,
    pub status: String,
    pub output: String,
    pub preview: String,
    pub seen: Vec<String>,
    pub pending_generate: Option<String>,
    pub pending_dryrun: bool,
    pub pending_execute: bool,
    pub pending_explain: bool,
    pub pending_copy: bool,
    pub quit: bool,
    /// danger 熔断门的信任表，由生产入口从 danger.toml 加载后注入；
    /// 状态机本身不做 IO，测试可注入自定义表。
    pub trust: TrustList,
}

impl App {
    /// 测试便利构造器：intent=None → 首屏输入框；有意图 → 直接进入生成。
    /// 生产路径必须走 `with_trust` 注入真实信任表，故本函数仅测试可见。
    #[cfg(test)]
    pub fn new(intent: Option<String>) -> App {
        Self::with_trust(intent, TrustList::default())
    }

    /// 生产入口：注入真实信任表，`[E]` 熔断时「信任命中视为 Clean」（设计 §7.3）。
    pub fn with_trust(intent: Option<String>, trust: TrustList) -> App {
        let (state, value, pending) = match intent {
            Some(i) => (State::Generating, i.clone(), Some(i)),
            None => (State::Input, String::new(), None),
        };
        App {
            state,
            intent: value,
            input: String::new(),
            candidates: Vec::new(),
            selected: 0,
            explanation: Vec::new(),
            status: String::new(),
            output: String::new(),
            preview: String::new(),
            seen: Vec::new(),
            pending_generate: pending,
            pending_dryrun: false,
            pending_execute: false,
            pending_explain: false,
            pending_copy: false,
            quit: false,
            trust,
        }
    }

    /// 测试/生成完成后装载候选（kb 组在前，LLM 组在后，不混排打分——决策 3）。
    pub fn load_candidates(&mut self, kb: Vec<String>, llm: Vec<String>, segments: Vec<Segment>) {
        let mut items: Vec<CandidateItem> = kb
            .into_iter()
            .map(|command| CandidateItem { command, group: Group::Kb })
            .collect();
        items.extend(
            llm.into_iter()
                .map(|command| CandidateItem { command, group: Group::Llm }),
        );
        if items.is_empty() {
            self.status = "没有可用候选，按 R 修正意图".to_string();
        }
        self.seen = items.iter().map(|c| c.command.clone()).collect();
        self.candidates = items;
        self.selected = 0;
        self.explanation = segments;
        self.state = State::Ready;
        self.preview.clear();
    }

    pub fn current_command(&self) -> &str {
        self.candidates
            .get(self.selected)
            .map(|c| c.command.as_str())
            .unwrap_or("")
    }

    /// §4.5 Refine 提示词拼装：原意图 + 当前命令 + 修正 + 已出现候选。
    pub fn refine_prompt(&self, refine_text: &str) -> String {
        format!(
            "原始意图：{}\n上一次生成：{}\n修正要求：{}\n（已出现过的候选不再重复：{}）",
            self.intent,
            self.current_command(),
            refine_text,
            self.seen.join(" | ")
        )
    }

    pub fn handle(&mut self, action: Action) {
        if action == Action::Quit {
            self.quit = true;
            return;
        }
        match (self.state, action) {
            (State::Input, Action::Char(c)) => self.input.push(c),
            (State::Input, Action::Backspace) => {
                self.input.pop();
            }
            (State::Input, Action::Enter) => {
                if !self.input.trim().is_empty() {
                    self.intent = self.input.trim().to_string();
                    self.input.clear();
                    self.state = State::Generating;
                    self.pending_generate = Some(self.intent.clone());
                }
            }
            (State::Generating, _) => {} // 生成中只接受 q
            (State::Ready, Action::Alternative) => {
                if !self.candidates.is_empty() {
                    self.selected = (self.selected + 1) % self.candidates.len();
                }
            }
            (State::Ready, Action::Refine) => {
                self.state = State::Refine;
                self.input.clear();
            }
            (State::Ready, Action::DryRun) => {
                self.state = State::DryRunPreview;
                self.pending_dryrun = true;
            }
            (State::Ready, Action::Explain) => {
                self.pending_explain = true;
                self.status = "正在重新获取逐段解释…".to_string();
            }
            (State::Ready, Action::Copy) => {
                self.pending_copy = true;
            }
            (State::Ready, Action::Execute) => {
                let verdict = danger::check(self.current_command(), &self.trust);
                self.state = match verdict.level {
                    danger::DangerLevel::Clean | danger::DangerLevel::Info => State::ConfirmExec,
                    danger::DangerLevel::Warning => State::ConfirmWarning,
                    danger::DangerLevel::Fatal => State::ConfirmFatal,
                };
                self.status = match self.state {
                    State::ConfirmFatal => "致命命令：请完整重新输入命令原文以确认".to_string(),
                    State::ConfirmWarning => format!("警告（{}）：确认执行? [y/N]", verdict.pattern.unwrap_or("")),
                    _ => "确认执行? [y/N]".to_string(),
                };
                self.input.clear();
            }
            (State::Refine, Action::Char(c)) => self.input.push(c),
            (State::Refine, Action::Backspace) => {
                self.input.pop();
            }
            (State::Refine, Action::Enter) => {
                let prompt = self.refine_prompt(self.input.trim());
                self.input.clear();
                self.state = State::Generating;
                self.pending_generate = Some(prompt);
            }
            (State::ConfirmExec | State::ConfirmWarning, Action::Char(c)) => self.input.push(c),
            (State::ConfirmExec | State::ConfirmWarning, Action::Enter) => {
                if self.input.trim().eq_ignore_ascii_case("y") {
                    self.pending_execute = true;
                    self.state = State::Executing;
                } else {
                    self.state = State::Ready;
                    self.status = "已取消执行".to_string();
                }
                self.input.clear();
            }
            (State::ConfirmFatal, Action::Char(c)) => self.input.push(c),
            (State::ConfirmFatal, Action::Backspace) => {
                self.input.pop();
            }
            (State::ConfirmFatal, Action::Enter) => {
                if self.input == self.current_command() {
                    self.pending_execute = true;
                    self.state = State::Executing;
                } else {
                    self.state = State::Ready;
                    self.status = "确认失败：命令原文不一致，已放弃".to_string();
                }
                self.input.clear();
            }
            (State::DryRunPreview | State::Executing | State::Done, Action::Esc) => {
                self.state = State::Ready;
                self.output.clear();
            }
            (_, Action::Esc) => self.state = State::Ready,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::danger::trust::TrustEntry;

    fn ready() -> App {
        let mut app = App::new(Some("意图".to_string()));
        app.load_candidates(
            vec!["kb1".to_string()],
            vec!["c1".to_string(), "c2".to_string()],
            Vec::new(),
        );
        app
    }

    /// T-U-63：连续两次 Alternative → 选中索引 1 → 2（轮询）
    #[test]
    fn tu63_alternative_cycles_candidates() {
        let mut app = App::new(None);
        app.load_candidates(
            Vec::new(),
            vec!["a".into(), "b".into(), "c".into()],
            Vec::new(),
        );
        assert_eq!(app.selected, 0);
        app.handle(Action::Alternative);
        assert_eq!(app.selected, 1);
        app.handle(Action::Alternative);
        assert_eq!(app.selected, 2);
        app.handle(Action::Alternative);
        assert_eq!(app.selected, 0);
    }

    /// T-U-64：fatal 命令 → ConfirmFatal，绝不直接进入 Executing
    #[test]
    fn tu64_execute_gates_fatal_command() {
        let mut app = ready();
        app.candidates = vec![CandidateItem {
            command: "rm -rf /".to_string(),
            group: Group::Llm,
        }];
        app.selected = 0;
        app.handle(Action::Execute);
        assert_eq!(app.state, State::ConfirmFatal);
        assert!(!app.pending_execute);
        // 错误原文 → 放弃
        app.input = "n".to_string();
        app.handle(Action::Enter);
        assert_eq!(app.state, State::Ready);
        // 完整原文 → 才放行
        app.handle(Action::Execute);
        app.input = "rm -rf /".to_string();
        app.handle(Action::Enter);
        assert_eq!(app.state, State::Executing);
        assert!(app.pending_execute);
    }

    /// T-U-65：Refine 拼装含原意图、当前命令、修正与 seen 列表
    #[test]
    fn tu65_refine_prompt_contains_context() {
        let mut app = ready();
        app.seen = vec!["c1".to_string(), "c2".to_string()];
        app.handle(Action::Refine);
        assert_eq!(app.state, State::Refine);
        for c in "只要 jpg".chars() {
            app.handle(Action::Char(c));
        }
        app.handle(Action::Enter);
        let prompt = app.pending_generate.clone().unwrap();
        assert!(prompt.contains("原始意图：意图"), "{prompt}");
        assert!(prompt.contains("上一次生成："), "{prompt}");
        assert!(prompt.contains("修正要求：只要 jpg"), "{prompt}");
        assert!(prompt.contains("c1 | c2"), "{prompt}");
        assert_eq!(app.state, State::Generating);
    }

    /// 设计 §7.3：Warning 级命令在信任命中时按 Clean 处理，
    /// 避免 `hlp --trust` 的授权在 TUI 内失效。
    #[test]
    fn trusted_warning_skips_warning_confirmation() {
        let warning = "rm -rf ./build".to_string();
        let mut app = App::new(Some("意图".to_string()));
        app.load_candidates(Vec::new(), vec![warning.clone()], Vec::new());

        // 未授权 → ConfirmWarning
        app.handle(Action::Execute);
        assert_eq!(app.state, State::ConfirmWarning);
        assert!(!app.pending_execute);

        // 授权后 → 直接 ConfirmExec（信任命中视为 Clean）
        let mut trusted = App::with_trust(
            Some("意图".to_string()),
            TrustList {
                entries: vec![TrustEntry {
                    pattern: "rm_recursive".to_string(),
                    trusted_at: 1,
                    example: warning.clone(),
                }],
            },
        );
        trusted.load_candidates(Vec::new(), vec![warning], Vec::new());
        trusted.handle(Action::Execute);
        assert_eq!(trusted.state, State::ConfirmExec);
        assert!(!trusted.pending_execute);
    }

    #[test]
    fn kb_group_is_placed_before_llm() {
        let app = ready();
        assert_eq!(app.candidates[0].group, Group::Kb);
        assert_eq!(app.candidates[1].group, Group::Llm);
        assert_eq!(app.current_command(), "kb1");
    }
}
