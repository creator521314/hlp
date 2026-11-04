//! bash 集成脚本模板 v2（v0.3 §4.4）。
//!
//! 相对 v0.2 的变化：
//! - 生成走 `hlp --stream`：每行一个全量快照，写入 READLINE_LINE
//!   （bind -x 下 readline 在函数返回后才重绘，视觉上等价"最终快照"）；
//! - 退出码经临时文件传递——`< <(...)` 的进程替换不向 `$?` 传值（遗留风险 #2）；
//! - 学习回流用 DEBUG trap（决策 7：bash 无 preexec，唯一可行方案），
//!   门控三重：`$hlp_generated` 非空 + 非 cache-learn 自身 + 跳过赋值语句，
//!   处理完立即清空，防复合命令重复触发与自激（遗留风险 #3）。
//!
//! 撤销 / 危险检查 / 信任机制与 v0.2 一致。

pub(crate) const SCRIPT: &str = r##"# hlp bash 集成 v2（流式 + 学习回流）—— 在 ~/.bashrc 加入：eval "$(hlp init bash)"
hlp-replace() {
  if [[ -n "$hlp_generated" && "$READLINE_LINE" == "$hlp_generated" ]]; then
    READLINE_LINE="$hlp_last_intent"
    READLINE_POINT=${#READLINE_LINE}
    hlp_generated=""
    echo "已撤销，恢复意图原文" >&2
    return
  fi
  local intent_line="$READLINE_LINE"
  local intent="${READLINE_LINE#//}"
  intent="${intent#"${intent%%[![:space:]]*}"}"
  if [[ -z "$intent" ]]; then echo "✗ 意图为空" >&2; return; fi
  echo "… hlp 生成中" >&2
  local last="" line rc rc_file
  rc_file="$(mktemp)"
  while IFS= read -r line; do
    last="$line"
    READLINE_LINE="$line"
    READLINE_POINT=${#READLINE_LINE}
  done < <( { hlp --stream "$intent" 2>/dev/null; echo "$?" >| "$rc_file"; } )
  rc="$(< "$rc_file")"
  rm -f "$rc_file"
  if [[ -z "$last" ]]; then
    READLINE_LINE="$intent_line"
    READLINE_POINT=${#READLINE_LINE}
    echo "✗ 生成失败（hlp 退出码 $rc）" >&2
    return
  fi
  local level pattern
  level=$(hlp --check "$last" 2>/dev/null | sed -n 's/^level=//p')
  pattern=$(hlp --check "$last" 2>/dev/null | sed -n 's/^pattern=//p')
  if [[ "$level" == "fatal" ]]; then
    local reply
    read -p "⚠ 致命命令，需完整重新输入以确认: $last
确认输入: " reply
    if [[ "$reply" != "$last" ]]; then
      READLINE_LINE="$intent_line"
      READLINE_POINT=${#READLINE_LINE}
      echo "✗ 确认失败，已放弃" >&2
      return
    fi
    hlp-confirm-trust "$pattern" "$last"
  elif [[ "$level" == "warning" ]]; then
    read -p "⚠ 警告（$pattern），回车采纳 / Ctrl+C 放弃: "
    hlp-confirm-trust "$pattern" "$last"
  fi
  hlp_last_intent="$intent_line"
  hlp_generated="$last"
  READLINE_LINE="$last"
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

# ── 学习回流：DEBUG trap（决策 7）。已知侵入性：每条命令前都会触发 ──
hlp-preexec() {
  [[ -z "$hlp_generated" ]] && return 0
  case "$BASH_COMMAND" in
    "hlp cache-learn"*) return 0;;   # 防自激
    *=*) return 0;;                  # 赋值语句不是用户执行的命令
  esac
  local executed="$BASH_COMMAND"
  local intent="${hlp_last_intent#//}"
  if [[ "$executed" == "$hlp_generated" ]]; then
    hlp cache-learn --intent "$intent" --command "$executed" --source adopted 2>/dev/null
  else
    hlp cache-learn --intent "$intent" --command "$executed" --source correction 2>/dev/null
  fi
  hlp_generated=""
  hlp_last_intent=""
  return 0
}
trap 'hlp-preexec' DEBUG

bind -x '"\C-g": hlp-replace'
"##;
