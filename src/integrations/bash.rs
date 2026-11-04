//! bash 集成脚本模板（设计 §3.3，逐字交付的协议）。
//!
//! `bind -x` 让函数运行在 readline 上下文，通过 READLINE_LINE / READLINE_POINT
//! 原地改写输入行；提示一律走 stderr（不能污染 buffer）。

pub(crate) const SCRIPT: &str = r##"# hlp bash 集成 —— 在 ~/.bashrc 加入：eval "$(hlp init bash)"
hlp-replace() {
  if [[ -n "$hlp_generated" && "$READLINE_LINE" == "$hlp_generated" ]]; then
    READLINE_LINE="$hlp_last_intent"
    READLINE_POINT=${#READLINE_LINE}
    hlp_generated=""
    echo "已撤销，恢复意图原文" >&2
    return
  fi
  local intent="${READLINE_LINE#//}"
  intent="${intent#"${intent%%[![:space:]]*}"}"
  if [[ -z "$intent" ]]; then echo "✗ 意图为空" >&2; return; fi
  echo "… hlp 生成中" >&2
  local cmd rc
  cmd=$(hlp --one-line "$intent" 2>/dev/null); rc=$?
  if [[ $rc -ne 0 || -z "$cmd" ]]; then echo "✗ 生成失败（hlp 退出码 $rc）" >&2; return; fi
  local level pattern
  level=$(hlp --check "$cmd" 2>/dev/null | sed -n 's/^level=//p')
  pattern=$(hlp --check "$cmd" 2>/dev/null | sed -n 's/^pattern=//p')
  if [[ "$level" == "fatal" ]]; then
    local reply
    read -p "⚠ 致命命令，需完整重新输入以确认: $cmd
确认输入: " reply
    if [[ "$reply" != "$cmd" ]]; then echo "✗ 确认失败，已放弃" >&2; return; fi
    hlp-confirm-trust "$pattern" "$cmd"
  elif [[ "$level" == "warning" ]]; then
    read -p "⚠ 警告（$pattern），回车采纳 / Ctrl+C 放弃: "
    hlp-confirm-trust "$pattern" "$cmd"
  fi
  hlp_last_intent="$READLINE_LINE"
  hlp_generated="$cmd"
  READLINE_LINE="$cmd"
  READLINE_POINT=${#READLINE_LINE}
}

hlp-confirm-trust() {
  local pattern="$1" cmd="$2"
  [[ -z "$pattern" ]] && return 1
  read -p "总是信任此模式（$pattern），以后不再提示? [y/N] " reply
  if [[ "$reply" == "y" || "$reply" == "Y" ]]; then
    hlp --trust "$pattern" --example "$cmd" 2>/dev/null && echo "已记忆信任（danger.toml 可撤销）" >&2
  fi
}

bind -x '"\C-g": hlp-replace'
"##;
