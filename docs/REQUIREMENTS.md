# hlp 需求文档

> 版本：v0.2 草案 | 日期：2026-11-04 | 状态：需求澄清后修订
>
> hlp —— 自然语言 → shell 命令翻译器：意图进，命令出，附逐段解释。

## 1. 项目背景

开发者经常遇到：脑中有明确**意图**（"把所有 jpg 缩到 800 宽"），却因不熟悉语法而无法组织出复杂的命令行（`find ... -exec convert ... \;`）。现有解决方案要么要求联网查搜索（割裂工作流），要么是独立的 AI 问答工具（生成结果需手动复制）。

hlp 的核心洞察：**缺的不是"执行"，而是"翻译"**。因此 hlp 定位为一个意图→命令的翻译器，叠加安全保护层，并**寄生在用户正在使用的 shell 输入行中**，而不是一个独立抢占终端的程序。

## 2. 命名与标识

| 元素 | 值 | 说明 |
|---|---|---|
| 程序名 | `hlp` | "help" 去元音，3 字母；crates.io 已验证无重名 |
| 行内前缀 | `// ` | 标记"这是意图，待翻译"；无 Shift、同键双击、误回车仅报 is a directory，无副作用 |
| 触发键 | `Ctrl+G` | G = Generate，将意图行原地替换为命令 |
| 反向模式 | `hlp -x '<命令>'` | eXplain：解释一条命令的含义 |

三者解耦，任何一层可单独通过配置修改。

被否决的命名及原因（存档）：

- `zao`（造）：读音不佳
- `gg`：crates.io 已占（2015 游戏库）；与 git 工具圈高频缩写冲突；中文语境"Google 一下"语义与产品理念相悖；"GG 了"负面含义
- `mantra` / `wiz` / `abra` / `yao` / `wand` / `plz`：crates.io 均已被占（含僵尸包，名字永久占用）

## 3. 输入规格

三种输入形态，按使用频率排序：

### 3.1 自然语言（主力）

```bash
hlp 把当前目录下所有超过100MB的文件列出来，按大小排序
```

- 支持中英混合、口语化描述
- 行内形态：输入 `// <意图>` 后按 Ctrl+G 触发翻译

### 3.2 反向解释（高频）

```bash
hlp -x "awk '{print $2}' access.log | sort | uniq -c | sort -rn | head"
```

- 解释他人博客里的命令、回顾自己过往的管道
- 输出按 `|` 拆段逐个解释

### 3.3 碎片补全

```bash
hlp -f "tar -xzvf"   # 记得半截，忘了后面跟什么
```

### 3.4 语音输入（远期）

- 全局热键 → 采音 → whisper.cpp（本地转写）→ 复用同一引擎
- 转写结果经剪贴板或 `xdotool` / `wtype` 注入
- 注意：ollama 不做语音，转写必须使用 whisper.cpp（GGUF 量化，CPU 可跑）

## 4. 输出规格

### 4.1 核心输出协议（决策：单行输出优先）

引擎核心输出为 **`--one-line` 单行纯文本**（面向 shell 替换集成），这是 v0.1 的主形态。富 TUI 卡片交互**延后到 v0.4** 再做。

`--one-line` 输出约定：

- stdout 仅输出一条可直接执行的命令（top1 候选），无任何装饰字符
- 解释、候选、警告等元信息走 stderr 或 `--verbose`，保证 shell 集成侧 `$(hlp --one-line ...)` 拿到的永远只有命令本身

### 4.2 多候选策略（决策：top1 + 可展开）

- 默认只给出 **top1 候选**（替换 buffer 时最快最简洁）
- 生成结果附带提示"还有 N 个备选"，按快捷键（如 Tab / `hlp --alt`）展开其余候选
- Refine 时优先从未展开的备选里取，避免重复

### 4.3 富交互卡片（v0.4）

```
┌─ 建议 1（推荐）────────────────────────────
│ find . -type f -size +100M -exec du -h {} + | sort -rh
│
│ 逐段解释:
│   find . -type f      递归找普通文件
│   -size +100M         大于100MB
│   du -h {} +          计算人类可读大小
│   sort -rh            按数值倒序
│
│ [D]ry-run  [C]opy  [E]xecute  [R]efine  [A]lternative
└────────────────────────────────────────────
```

设计原则：

1. **逐段解释**：管道命令按 `|` 拆开逐段讲解——输出本身即教学，用多了自然学会
2. **Dry-run 优先**：能模拟执行的命令（`echo` 替换、`-n`、`--dry-run`）先跑一遍给结果预览
3. **危险命令熔断**：见 §7 安全设计
4. **Refine 循环**：结果不对时输入一句"不对，只要 jpg"，多轮修正
5. **解释语言跟随意图语言**（决策）：意图是中文则中文解释，英文则英文解释，不做全局固定

## 5. 核心交互：shell 输入行原地替换

这是 hlp 与所有竞品的核心差异点。体验流程：

```
$ // 把当前目录所有超过100MB的文件按大小排序列出来
      ↓ Ctrl+G
$ find . -type f -size +100M -exec du -h {} + | sort -rh   ← 光标在末尾
      ↓ 用户可继续手动微调，回车才执行（执行权永远在用户）
```

配套交互：

- **不自动执行**：替换后由用户回车确认
- **撤销替换**：再按一次 Ctrl+G 恢复原始意图文字
- 误触前缀直接回车：`//xxx` 只会报 "is a directory"，无副作用

### 5.1 shell 集成范围（决策：v0.2 三种 shell 全覆盖）

zsh / bash / fish 三种集成在 v0.2 同一版本内全部交付。

**zsh（ZLE widget）：**

```zsh
hlp-replace() {
  local intent="${BUFFER#//}"           # 去掉 // 前缀
  local cmd=$(/path/to/hlp --one-line "$intent")
  if [[ -n "$cmd" ]]; then
    BUFFER="$cmd"
    CURSOR=${#BUFFER}
    zle -M "✓ 已生成 (Ctrl+G 撤销)"
  else
    zle -M "✗ 生成失败"
  fi
}
zle -N hlp-replace
bindkey '^G' hlp-replace
```

**bash（bind -x + READLINE_LINE）：**

```bash
hlp-replace() {
  READLINE_LINE=$(hlp --one-line "$READLINE_LINE")
  READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-g": hlp-replace'
```

**fish（commandline 内置）：**

```fish
function hlp-replace
  set line (commandline)
  commandline --replace (hlp --one-line "$line")
end
bind \cg hlp-replace
```

关键分层：**Rust 引擎只负责 stdin/参数 → stdout 单行命令**，shell 集成层薄到几行配置，引擎可复用于任何前端（含语音）。

## 6. 架构：三层缓存 + 安全层

"替换输入行"意味着用户在对话中等待，延迟是命门。策略：**把 LLM 当最慢的兜底层**。

```
用户输入
   ↓
[第 1 层] 历史缓存           < 10ms   命中即复用；含人工修正回流
   ↓ 未命中
[第 2 层] 本地知识库          < 50ms   tldr-pages 任务卡片 + man 结构化索引
   ↓ 置信度低
[第 3 层] LLM Provider 插件   1~3s    ollama 为默认实现；插件接口可插拔
   ↓
[安全检查层]                          危险模式匹配 / Dry-run 预览 / 二次确认
   ↓
输出：命令 + 逐段解释 → 写回 shell buffer / TUI
      用户采纳或修正 → 回流第 1 层缓存
```

### 6.1 延迟预算

| 路径 | 预算 | 手段 |
|---|---|---|
| 缓存命中 | < 10ms | SQLite 本地查询 |
| 知识库命中 | < 50ms | tldr JSON 索引 + 关键词匹配（初期不上向量检索） |
| LLM 生成 | 感知 < 500ms | 小模型 + 常驻 daemon + 流式替换（首 token 即写入 buffer） |

### 6.2 压延迟的四个手段

1. **小模型**：ollama 跑 `qwen2.5-coder:1.5b/3b`，窄任务足够且快
2. **常驻 daemon**（决策：按需拉起 + 空闲超时退出）：首次调用自动拉起 daemon，闲置 **5 分钟**自动退出，不做 systemd 常驻、不占常驻内存；后续调用走本地 unix socket，消除进程冷启动
3. **流式替换**：LLM 吐第一个 token 就开始写 buffer，"命令正在打字"的视觉，感知延迟趋近零
4. **前缀缓存**：部分匹配的历史意图直接复用

### 6.3 LLM 后端（决策：插件化 Provider 接口）

- 引擎内部抽象 `Provider` trait（`generate(intent) -> candidates`）
- 内置默认实现：本地 ollama（HTTP `/api/chat`，流式）
- 后续可插拔：OpenAI 兼容端点（DeepSeek/智谱等）、其他本地推理后端
- 配置项：`~/.config/hlp/config.toml` 中 `provider = "ollama"`（默认）

## 7. 安全设计

### 7.1 危险模式清单（初版）

| 模式 | 级别 | 处置 |
|---|---|---|
| `rm -rf` / `rm -fr`（尤其指向 `/`、`~`、`*`） | 致命 | 强制高亮 + 输入完整命令原文二次确认 |
| `dd of=/dev/...` | 致命 | 同上 |
| `mkfs*` | 致命 | 同上 |
| `> <file>`（重定向覆盖） | 警告 | 高亮提示 + 确认 |
| `chmod -R 777 /`、`chown -R` 指向系统目录 | 警告 | 高亮提示 |
| `curl ... \| sh` / `wget ... \| bash` | 警告 | 提示来源不可信 |
| `kill -9` 无目标限定 | 提示 | 轻提示 |

### 7.2 信任机制（决策：可记忆信任）

- 危险命令触发确认时提供两级选项：**"仅这次"** / **"总是（此模式）"**
- 选"总是"后，该危险模式写入 `~/.config/hlp/danger.toml` 的信任列表，后续不再提示
- 信任规则可在配置文件中手动查看和撤销

### 7.3 原则

- **绝不自动执行**任何生成结果；执行动作永远由用户回车/确认触发
- Dry-run 能力优先于真实执行
- 危险清单可配置（`~/.config/hlp/danger.toml`）
- 提示词层面同时注入安全规则，双保险

## 8. 隐私与数据（决策）

- 历史缓存**默认开启**，记录意图 → 命令的映射与采纳/修正结果
- `hlp --clear` 一键清除全部历史
- **目录级豁免**：缓存配置支持路径豁免列表（如 `~/secrets`），在豁免目录下不记录任何条目
- 所有数据仅存本地，纯本地 ollama 为默认后端，不外发任何数据

## 9. 技术选型

| 组件 | 选择 | 理由 |
|---|---|---|
| 语言 | Rust | 快启动、单二进制、便于发 AUR |
| TUI | ratatui（v0.4 引入） | 成熟生态 |
| LLM 调用 | Provider 插件接口，默认 ollama 实现 | 可插拔、无 SDK 强依赖、流式 |
| 知识库 | tldr-pages JSON（MIT） | 社区维护的任务卡片白拿 |
| 存储 | SQLite（rusqlite） | 历史 + 采纳记录 |
| 配置 | TOML（`~/.config/hlp/`） | config.toml + danger.toml |
| 分发 | cargo / crates.io / AUR | 单二进制 |

## 10. 版本路线（澄清后修订）

| 版本 | 内容 | 验收标准 |
|---|---|---|
| **v0.1** | 引擎：参数 → Provider(ollama) → stdout 单行命令（`--one-line`）；Provider trait 定义 | `hlp --one-line "列出大文件"` 输出可用命令；stdout 纯净无装饰 |
| **v0.2** | **zsh / bash / fish 三种 shell 集成** + `//` 前缀 + Ctrl+G 替换；危险命令检测 + 可记忆信任 | 三种 shell 各自完成一次完整替换流程 |
| **v0.3** | daemon（按需拉起 + 空闲 5 分钟超时退出）+ unix socket + 流式替换；历史缓存（默认开 + `--clear` + 目录豁免） | 缓存命中亚秒返回；daemon 闲置自动退出 |
| **v0.4** | tldr 知识库（第 2 层）；`hlp -x` 解释模式（语言跟随意图）；Dry-run；TUI 卡片（top1 + 可展开候选） | 全部三层打通 |
| 远期 | 语音输入（whisper.cpp）、配置管理子命令 | — |

## 11. 竞品分析与差异化

| 项目 | 形态 | hlp 的差异 |
|---|---|---|
| GitHub Copilot CLI | 云端 API，选择列表式 | hlp 本地化、buffer 原地替换 |
| ai-shell (Builder.io) | 独立程序 + 确认流 | 同上 |
| caro | 本地 LLM（最接近） | hlp 有 shell 集成 + 三层缓存 |
| plz-cli / sai / how2 | 独立 CLI / 检索式 | hlp 有学习闭环 + 解释模式 |
| thefuck | 事后纠正上一条命令 | hlp 是事前生成 |
| Warp | 整个 AI 终端 | hlp 给现有 shell 加能力，不换终端 |

**无人区**：shell buffer 原地替换、三层缓存压延迟、修正回流学习闭环、生成/解释双向集成于同一触发器。

## 12. 非目标（明确不做）

- 不做终端复用器/替代终端（Warp 路线）
- v0.x 不做 vim/neovim 集成
- 不做命令自动执行（永远用户确认）
- 默认实现不依赖云端 API（云端仅作为 Provider 插件的可选项）

## 13. 决策记录

| 日期 | 问题 | 决策 |
|---|---|---|
| 2026-11-04 | v0.1 输出形态 | 单行输出优先，TUI 卡片延后到 v0.4 |
| 2026-11-04 | shell 集成范围 | v0.2 一次性覆盖 zsh / bash / fish |
| 2026-11-04 | LLM 后端 | 插件化 Provider 接口，ollama 为默认实现 |
| 2026-11-04 | 解释语言 | 跟随意图语言（中文意图→中文解释） |
| 2026-11-04 | 多候选 | top1 默认 + 可展开备选 |
| 2026-11-04 | 危险命令信任 | 可记忆信任（仅这次 / 总是此模式，写入 danger.toml 可撤销） |
| 2026-11-04 | daemon 生命周期 | 按需拉起 + 空闲 5 分钟超时退出，不做 systemd 常驻 |
| 2026-11-04 | 缓存隐私 | 默认开启 + `hlp --clear` 清除 + 目录级豁免 |

## 14. 开放问题

- [ ] 缓存的意图匹配策略：精确 / 模糊 / 前缀，阈值如何定
- [ ] tldr 卡片如何与 LLM 输出融合排序（置信度评分）
- [ ] Provider trait 的候选数量上限与超时策略
