//! zsh 集成脚本模板 v2（v0.3 §4.4）。
//!
//! 相对 v0.2 的变化：
//! - 生成走 `hlp --stream`：每行一个全量快照，buffer 逐行刷新（"命令正在打字"）；
//! - 退出码经临时文件传递——`< <(...)` 的进程替换不向 `$?` 传值（遗留风险 #2）；
//! - `add-zsh-hook preexec` 学习回流：执行了 hlp 生成/修正的命令 → cache-learn。
//!
//! 撤销 / 危险检查 / 信任机制与 v0.2 逐字一致。

pub(crate) const SCRIPT: &str = r##"# hlp zsh 集成 v2（流式 + 学习回流）—— 在 ~/.zshrc 加入：eval "$(hlp init zsh)"
hlp-replace() {
  # ── 撤销：当前 buffer 是上次生成结果 → 恢复原始意图 ──
  if [[ -n "$hlp_generated" && "$BUFFER" == "$hlp_generated" ]]; then
    BUFFER="$hlp_last_intent"
    CURSOR=${#BUFFER}
    hlp_generated=""
    zle -M "已撤销，恢复意图原文"
    return
  fi
  local intent_line="$BUFFER"
  local intent="${BUFFER#//}"
  intent="${intent#"${intent%%[![:space:]]*}"}"   # 去前导空白
  if [[ -z "$intent" ]]; then
    zle -M "✗ 意图为空"
    return
  fi
  zle -M "… hlp 生成中"
  zle reset-prompt
  # ── 流式：每行一个全量快照，逐行刷 buffer ──
  # 进程替换拿不到 hlp 的退出码 → 写入临时文件，EOF 前保证已写入
  local last="" line rc_file rc
  rc_file="$(mktemp "${TMPDIR:-/tmp}/hlp-rc.XXXXXX")"
  while IFS= read -r line; do
    last="$line"
    BUFFER="$line"
    CURSOR=${#BUFFER}
    zle reset-prompt
  done < <( { hlp --stream "$intent" 2>/dev/null; echo "$?" >| "$rc_file"; } )
  rc="$(< "$rc_file")"
  command rm -f "$rc_file"
  if [[ -z "$last" ]]; then
    BUFFER="$intent_line"
    CURSOR=${#BUFFER}
    zle -M "✗ 生成失败（hlp 退出码 $rc）"
    return
  fi
  # ── 危险检查（本地，<10ms）──
  local level pattern
  level=$(hlp --check "$last" 2>/dev/null | sed -n 's/^level=//p')
  pattern=$(hlp --check "$last" 2>/dev/null | sed -n 's/^pattern=//p')
  if [[ "$level" == "fatal" ]]; then
    local reply
    read "reply?⚠ 致命命令，需完整重新输入以确认: $last
确认输入: "
    if [[ "$reply" != "$last" ]]; then
      BUFFER="$intent_line"
      CURSOR=${#BUFFER}
      zle -M "✗ 确认失败，已放弃（原始意图保留）"
      return
    fi
    if hlp-confirm-trust "$pattern" "$last"; then :; fi
  elif [[ "$level" == "warning" ]]; then
    local reply
    read "reply?⚠ 警告（$pattern），回车采纳 / Ctrl+C 放弃: "
    hlp-confirm-trust "$pattern" "$last"
  fi
  hlp_last_intent="$intent_line"
  hlp_generated="$last"
  BUFFER="$last"
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

# ── 学习回流：执行了 hlp 生成/修正的命令 → 写入历史缓存 ──
hlp-preexec() {
  [[ -z "$hlp_generated" ]] && return
  local executed="$1"
  local intent="${hlp_last_intent#//}"
  if [[ "$executed" == "$hlp_generated" ]]; then
    hlp cache-learn --intent "$intent" --command "$executed" --source adopted 2>/dev/null
  else
    hlp cache-learn --intent "$intent" --command "$executed" --source correction 2>/dev/null
  fi
  hlp_generated=""
  hlp_last_intent=""
}

zle -N hlp-replace
bindkey '^G' hlp-replace
autoload -Uz add-zsh-hook
add-zsh-hook preexec hlp-preexec
"##;
