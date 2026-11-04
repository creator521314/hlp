//! fish 集成脚本模板 v2（v0.3 §4.4）。
//!
//! 相对 v0.2 的变化：
//! - 生成走 `hlp --stream`：每行一个全量快照，`commandline --replace` 逐行刷新；
//! - 退出码用 `$pipestatus[1]`（fish 管道不产生子 shell，变量在循环后可见）；
//! - `fish_preexec` 事件学习回流。

pub(crate) const SCRIPT: &str = r##"# hlp fish 集成 v2（流式 + 学习回流）—— 在 ~/.config/fish/config.fish 加入：hlp init fish | source
function hlp-replace --description 'hlp: 意图 → 命令'
    # 撤销
    if test -n "$hlp_generated"; and test (commandline) = "$hlp_generated"
        commandline --replace "$hlp_last_intent"
        set -e hlp_generated
        echo "已撤销，恢复意图原文" >&2
        return
    end
    set -l intent_line (commandline)
    set -l intent (string replace --regex '^//' '' (commandline) | string trim -l)
    if test -z "$intent"
        echo "✗ 意图为空" >&2; return
    end
    echo "… hlp 生成中" >&2
    # 流式：每行一个全量快照，逐行刷 commandline
    set -l last ""
    hlp --stream "$intent" 2>/dev/null | while read -r line
        set last "$line"
        commandline --replace "$line"
        commandline --cursor (string length "$line")
    end
    set -l rc $pipestatus[1]
    if test -z "$last"
        commandline --replace "$intent_line"
        echo "✗ 生成失败（hlp 退出码 $rc）" >&2; return
    end
    set -l level (hlp --check "$last" 2>/dev/null | string match --regex '^level=(.*)$')[2]
    set -l pattern (hlp --check "$last" 2>/dev/null | string match --regex '^pattern=(.*)$')[2]
    if test "$level" = "fatal"
        read -p "echo '⚠ 致命命令，需完整重新输入以确认: $last
确认输入: '" reply
        if test "$reply" != "$last"
            commandline --replace "$intent_line"
            echo "✗ 确认失败，已放弃" >&2; return
        end
        hlp-confirm-trust "$pattern" "$last"
    else if test "$level" = "warning"
        read -p "echo '⚠ 警告（'$pattern'），回车采纳 / Ctrl+C 放弃: '"
        hlp-confirm-trust "$pattern" "$last"
    end
    set -g hlp_last_intent "$intent_line"
    set -g hlp_generated "$last"
    commandline --replace "$last"
    commandline --cursor (string length "$last")
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

# ── 学习回流：执行了 hlp 生成/修正的命令 → 写入历史缓存 ──
function hlp-preexec --on-event fish_preexec
    test -z "$hlp_generated"; and return
    set -l executed $argv[1]
    set -l intent (string replace --regex '^//' '' "$hlp_last_intent")
    if test "$executed" = "$hlp_generated"
        hlp cache-learn --intent "$intent" --command "$executed" --source adopted 2>/dev/null
    else
        hlp cache-learn --intent "$intent" --command "$executed" --source correction 2>/dev/null
    end
    set -e hlp_generated
    set -e hlp_last_intent
end

bind \cg hlp-replace
"##;
