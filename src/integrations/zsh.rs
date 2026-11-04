//! zsh 集成脚本模板（设计 §3.3，逐字交付的协议——文案与 `hlp` 调用点不得改写）。
//!
//! 交互状态机见设计 §4.1：撤销 → 生成 → 危险检查 → 确认 → 替换。

pub(crate) const SCRIPT: &str = r##"# hlp zsh 集成 —— 在 ~/.zshrc 加入：eval "$(hlp init zsh)"
hlp-replace() {
  # ── 撤销：当前 buffer 是上次生成结果 → 恢复原始意图 ──
  if [[ -n "$hlp_generated" && "$BUFFER" == "$hlp_generated" ]]; then
    BUFFER="$hlp_last_intent"
    CURSOR=${#BUFFER}
    hlp_generated=""
    zle -M "已撤销，恢复意图原文"
    return
  fi
  local intent="${BUFFER#//}"
  intent="${intent#"${intent%%[![:space:]]*}"}"   # 去前导空白
  if [[ -z "$intent" ]]; then
    zle -M "✗ 意图为空"
    return
  fi
  zle -M "… hlp 生成中"
  zle reset-prompt
  local cmd rc
  cmd=$(hlp --one-line "$intent" 2>/dev/null); rc=$?
  if [[ $rc -ne 0 || -z "$cmd" ]]; then
    zle -M "✗ 生成失败（hlp 退出码 $rc）"
    return
  fi
  # ── 危险检查（本地，<10ms）──
  local level pattern
  level=$(hlp --check "$cmd" 2>/dev/null | sed -n 's/^level=//p')
  pattern=$(hlp --check "$cmd" 2>/dev/null | sed -n 's/^pattern=//p')
  if [[ "$level" == "fatal" ]]; then
    local reply
    read "reply?⚠ 致命命令，需完整重新输入以确认: $cmd
确认输入: "
    if [[ "$reply" != "$cmd" ]]; then
      zle -M "✗ 确认失败，已放弃（原始意图保留）"
      return
    fi
    if hlp-confirm-trust "$pattern" "$cmd"; then :; fi
  elif [[ "$level" == "warning" ]]; then
    local reply
    read "reply?⚠ 警告（$pattern），回车采纳 / Ctrl+C 放弃: "
    hlp-confirm-trust "$pattern" "$cmd"
  fi
  hlp_last_intent="$BUFFER"
  hlp_generated="$cmd"
  BUFFER="$cmd"
  CURSOR=${#BUFFER}
  zle -M "✓ 已生成（再按 Ctrl+G 撤销，回车执行）"
}

# 危险确认后询问是否记忆信任（fatal/warning 共用）
hlp-confirm-trust() {
  local pattern="$1" cmd="$2"
  [[ -z "$pattern" ]] && return 1
  read "reply?总是信任此模式（$pattern），以后不再提示? [y/N] "
  if [[ "$reply" == "y" || "$reply" == "Y" ]]; then
    hlp --trust "$pattern" --example "$cmd" 2>/dev/null \
      && zle -M "已记忆信任（danger.toml 可撤销）"
  fi
}

zle -N hlp-replace
bindkey '^G' hlp-replace
"##;
