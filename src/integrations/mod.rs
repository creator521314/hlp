//! shell 集成脚本模板与分发（设计 §2.4 / §3.3）。
//!
//! 硬边界（设计决策 4）：**不写用户 rc 文件**，只把脚本打到 stdout，
//! 由用户 `eval "$(hlp init zsh)"` 消费。
//!
//! 模板内的 `hlp` 一律是裸命令名（设计决策 7）：依赖 PATH，不内嵌自身 exe 绝对路径，
//! 这样 `cargo install --force` 后不会失效。

mod bash;
mod fish;
mod zsh;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    /// 解析子命令参数。非法值在 clap 层已被 value_parser 拦掉（退出码 2），
    /// 这里返回 None 仅作防御。
    pub fn from_arg(s: &str) -> Option<Shell> {
        match s {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }
}

/// 返回完整集成脚本（设计 §3.3 模板全文，逐字交付）。
/// stdout 除脚本外**不得有任何其他输出**，否则 `eval` 会执行到意外内容。
pub fn script(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh => zsh::SCRIPT,
        Shell::Bash => bash::SCRIPT,
        Shell::Fish => fish::SCRIPT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_arg_accepts_three_shells_only() {
        assert_eq!(Shell::from_arg("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_arg("bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_arg("fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_arg("sh"), None);
        assert_eq!(Shell::from_arg(""), None);
    }

    /// 每份脚本都必须：绑定 Ctrl+G（^G）、流式生成、危险检查、信任、学习回流
    #[test]
    fn scripts_bind_ctrl_g_and_call_hlp() {
        for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
            let s = script(shell);
            assert!(!s.is_empty());
            assert!(s.ends_with('\n'), "{shell:?} 脚本应以换行结尾");
            for needle in ["hlp --stream", "hlp --check", "hlp --trust", "hlp cache-learn", "//"] {
                assert!(s.contains(needle), "{shell:?} 脚本缺少 {needle}");
            }
        }
        assert!(script(Shell::Zsh).contains("bindkey '^G'"));
        assert!(script(Shell::Bash).contains("bind -x '\""), "bash 用 bind -x + \\C-g");
        assert!(script(Shell::Bash).contains(r"\C-g"));
        assert!(script(Shell::Fish).contains(r"bind \cg"));
    }

    /// v0.3 新增：三 shell 的 preexec 学习钩子（T-I-31 的单元级前置）
    #[test]
    fn scripts_have_preexec_hooks() {
        assert!(script(Shell::Zsh).contains("add-zsh-hook preexec"));
        assert!(script(Shell::Bash).contains("trap 'hlp-preexec' DEBUG"));
        assert!(script(Shell::Fish).contains("--on-event fish_preexec"));
        // 退出码传递：进程替换陷阱的规避手段
        assert!(script(Shell::Zsh).contains("rc_file"), "zsh 用临时文件传退出码");
        assert!(script(Shell::Bash).contains("rc_file"), "bash 用临时文件传退出码");
        assert!(script(Shell::Fish).contains("pipestatus"), "fish 用 pipestatus");
    }
}
