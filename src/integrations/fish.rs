//! fish 集成脚本模板（设计 §3.3，逐字交付的协议）。
//!
//! fish 用 `commandline --replace` / `--cursor` 改写输入行；
//! 状态用 `set -g` 全局变量保存（会话级，与 bash/zsh 的语义对齐）。

pub(crate) const SCRIPT: &str = r##"# hlp fish 集成 —— 在 ~/.config/fish/config.fish 加入：hlp init fish | source
function hlp-replace --description 'hlp: 意图 → 命令'
    # 撤销
    if test -n "$hlp_generated"; and test (commandline) = "$hlp_generated"
        commandline --replace "$hlp_last_intent"
        set -e hlp_generated
        echo "已撤销，恢复意图原文" >&2
        return
    end
    set -l intent (string replace --regex '^//' '' (commandline) | string trim -l)
    if test -z "$intent"
        echo "✗ 意图为空" >&2; return
    end
    echo "… hlp 生成中" >&2
    set -l cmd (hlp --one-line "$intent" 2>/dev/null)
    if test $status -ne 0; or test -z "$cmd"
        echo "✗ 生成失败" >&2; return
    end
    set -l level (hlp --check "$cmd" 2>/dev/null | string match --regex '^level=(.*)$')[2]
    set -l pattern (hlp --check "$cmd" 2>/dev/null | string match --regex '^pattern=(.*)$')[2]
    if test "$level" = "fatal"
        read -p "echo '⚠ 致命命令，需完整重新输入以确认: $cmd
确认输入: '" reply
        if test "$reply" != "$cmd"
            echo "✗ 确认失败，已放弃" >&2; return
        end
        hlp-confirm-trust "$pattern" "$cmd"
    else if test "$level" = "warning"
        read -p "echo '⚠ 警告（'$pattern'），回车采纳 / Ctrl+C 放弃: '"
        hlp-confirm-trust "$pattern" "$cmd"
    end
    set -g hlp_last_intent (commandline)
    set -g hlp_generated "$cmd"
    commandline --replace "$cmd"
    commandline --cursor (string length "$cmd")
end

function hlp-confirm-trust
    set -l pattern $argv[1]
    set -l cmd $argv[2]
    test -z "$pattern"; and return 1
    read -p "echo '总是信任此模式（'$pattern'），以后不再提示? [y/N] '" reply
    if test "$reply" = "y"; or test "$reply" = "Y"
        hlp --trust "$pattern" --example "$cmd" 2>/dev/null; and echo "已记忆信任" >&2
    end
end

bind \cg hlp-replace
"##;
