# hlp

自然语言 → shell 命令翻译器。意图进，命令出，附逐段解释。

```bash
$ hlp 把当前目录下所有超过100MB的文件按大小排序
find . -type f -size +100M -exec du -h {} + | sort -rh
```

v0.2 起支持 shell 输入行原地替换：`// ` 前缀 + Ctrl+G，命令直接落回输入行，回车才执行。
v0.3 起生成过程**流式打字**（逐快照刷新输入行），命令执行后自动学习回流进历史缓存，
下次同样意图直接从缓存出（毫秒级，不再走 LLM）。
v0.4 起接入 **tldr 知识库**（第 2 层）、`hlp -x` 逐段解释、只读 `--dry-run` 预览，
以及 `--tui` 富交互卡片（top1 + 可展开候选 + 快捷键）。

```
$ // 把所有超过100MB的文件按大小列出来
      ↓ Ctrl+G（流式：命令像打字一样逐字浮现）
$ find . -type f -size +100M -exec du -h {} + | sort -rh   ← 光标在末尾
```

- 纯本地：历史缓存（第 1 层）→ tldr 知识库（第 2 层）→ ollama 小模型（第 3 层），
  命中前两层时零 LLM 请求
- 安全：危险命令熔断、可记忆信任、永不自动执行；dry-run 只跑只读预览命令
- 学习型：逐段解释 + 采纳/修正自动回流缓存

详细需求见 [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)，设计文档见
[docs/design/v0.1-engine.md](docs/design/v0.1-engine.md)、
[docs/design/v0.2-shell-integration.md](docs/design/v0.2-shell-integration.md)、
[docs/design/v0.3-daemon-cache.md](docs/design/v0.3-daemon-cache.md)、
[docs/design/v0.4-tui-knowledge.md](docs/design/v0.4-tui-knowledge.md)。

## 当前状态：v0.4（知识库 + 解释 + Dry-run + TUI 卡片）

已实现：

- v0.1：命令行参数 → Provider(ollama) → stdout 单行命令
- v0.2：zsh / bash / fish 三种集成（`//` + Ctrl+G 原地替换、再按撤销）；
  危险命令分级检测（`hlp --check`）+ 可记忆信任（`hlp --trust` → `danger.toml`）
- v0.3：后台 daemon（unix socket，首次使用自动拉起、空闲自退出）+ SQLite 历史缓存
  （精确/前缀匹配、采纳与修正回流、`--clear` 清空、`exempt_dirs` 目录豁免）；
  shell 集成模板 v2——`hlp --stream` 流式替换 + preexec 学习回流
- v0.4：tldr 知识库（`kb-update` 显式导入、`kb-status` 查询、关键词评分阈值短路）；
  `hlp -x` 解释模式（引号/转义感知拆段、语言跟随 locale、LLM 综合 + 本地标注兜底）；
  `--dry-run` 只读预览（argv 直执、5s 超时）；`--tui` 卡片（候选分组、danger 门、OSC52 复制）；
  daemon 协议升 v2（Explain op）

### 安装与前置

- Rust 工具链（stable）
- 本地 [ollama](https://ollama.com) 已运行，且已拉取默认模型：
  ```bash
  ollama pull qwen2.5-coder:3b
  ```

### shell 集成

`hlp init <shell>` 只把脚本打到 stdout，**不写你的 rc 文件**；由你自己放进启动文件：

```bash
# zsh —— ~/.zshrc
eval "$(hlp init zsh)"

# bash —— ~/.bashrc
eval "$(hlp init bash)"

# fish —— ~/.config/fish/config.fish
hlp init fish | source
```

重新加载后：输入 `// 意图`，按 **Ctrl+G**，输入行原地变成命令（流式：逐快照刷新，
像命令正在打字；光标在末尾）。不回车就不会执行——hlp 全程不调用任何 exec，
执行权只属于你的回车。

| 操作 | 结果 |
|---|---|
| Ctrl+G | 流式生成并替换输入行 |
| 再按 Ctrl+G | 撤销，恢复原始意图文字（再按又生成） |
| 回车执行生成命令 | preexec 钩子把（意图 → 命令）回流进缓存（adopted） |
| 回车执行修改后的命令 | 回流为修正样本（correction），覆盖旧候选 |
| 直接回车（未按 Ctrl+G） | shell 原生行为，`//xxx` 只会报 `is a directory` |

学习回流的采集由各 shell 的 preexec 钩子完成：zsh 用 `add-zsh-hook preexec`、
fish 用 `fish_preexec` 事件、bash 无 preexec，用 DEBUG trap 模拟（见"已知局限"）。

### 危险命令熔断

生成后立刻本地检测（<10ms，不走 LLM），分三档：

| 级别 | 例子 | 处置 |
|---|---|---|
| fatal | `rm -rf /`、`rm -rf ~`、`dd of=/dev/sda`、`mkfs.ext4 ...` | 必须**完整重新输入该命令原文**才采纳 |
| warning | `rm -rf ./build`、`echo x > file`、`chmod -R 777 /etc`、`curl ... \| sh` | 回车采纳 / Ctrl+C 放弃，采纳后可选择"总是信任" |
| info | `kill -9` 无目标 | 轻提示，不阻塞 |

8 条内置规则 ID（也是 `--trust` 接受的取值）：

```
rm_recursive_root  dd_device  mkfs  rm_recursive
redirect_overwrite  chmod_recursive  pipe_to_shell  kill_9_unscoped
```

**信任机制**：选"总是信任此模式"后写入 `~/.config/hlp/danger.toml`，同 ID 以后不再提示。
撤销信任 = 手动删掉对应 `[[trust]]` 条目（v0.2 不提供 `--untrust`，见设计决策 10）：

```toml
# ~/.config/hlp/danger.toml（hlp --trust 是唯一写者）
[[trust]]
pattern = "rm_recursive"        # 规则 ID
trusted_at = 1793836800         # Unix 秒
example = "rm -rf ./build"      # 触发时的样例命令，便于人工审查
```

也可以命令行直接写：

```bash
hlp --check "rm -rf /"                              # 本地检测，不走 LLM
hlp --trust rm_recursive --example "rm -rf ./build" # 记忆信任
```

`--check` 的 stdout 是给 shell 解析的 `key=value` 协议（键序固定，值内无换行）：

```
level=warning
pattern=rm_recursive
reason=rm 递归删除普通路径
```

### 历史缓存与 daemon（v0.3）

**响应路径**：每次生成先查本地 SQLite 缓存（精确匹配，不足 4 字符的意图退化为
最长前缀匹配），命中则毫秒级返回；未命中走 daemon → ollama，同时流式打字。

- **缓存位置**：`~/.local/share/hlp/cache.db`（WAL 模式，权限 0600）
- **学习来源**：shell 集成自动回流（采纳 adopted / 修正 correction）；也可手动写：
  ```bash
  hlp cache-learn --intent "列出大文件" --command "du -ah . | sort -rh" --source adopted
  ```
- **清空**：
  ```bash
  hlp --clear
  ```
  缓存故障时静默降级直连 LLM；`--clear` / `cache-learn` 显式操作缓存失败则退出码 6。
- **目录豁免**：`[cache] exempt_dirs` 里列出的目录（词法前缀匹配，支持 `~`）内
  既不查缓存也不学习——临时目录、密钥目录等不该留痕。

**daemon**：首次生成时自动后台拉起（`hlp daemon`），单实例（flock 保证），空闲
`idle_secs` 后自动退出并清理 socket；挂了也无感——客户端自动回退直连 LLM。
无需手动管理；socket 默认在 `$XDG_RUNTIME_DIR/hlp.sock`，可用 `[daemon] socket_path` 自定义。

`--verbose` 可看到来源标注：

```
$ hlp --verbose --one-line "列出大文件"     # 第二次同样意图
provider=ollama model=qwen2.5-coder:3b candidates=1 elapsed=4ms source=cache match=exact
du -ah . | sort -rh
```

### tldr 知识库（v0.4，第 2 层）

生成查找链：**历史缓存 → tldr 知识库 → daemon/LLM**，前两层命中即短路返回，
零 LLM 请求（毫秒级）。知识库语料与历史缓存同库（`hlp.sqlite`），但
`hlp --clear` **只清历史缓存，不清公共语料**。

```bash
hlp kb-update     # 显式联网：下载官方 tldr.zip → 解压 → 事务重建语料（进度在 stderr）
hlp kb-status     # pages=N examples=N updated_at=<Unix 秒>
```

- 语料含英文 `pages/` 与中文 `pages.zh/` 的 common/linux 目录；中文意图同时匹配中英语料，
  英文意图只匹配英文语料
- 命中判定是关键词召回评分（阈值 `[kb] min_score`，默认 0.6），不是语义/向量检索；
  达阈值的示例按分数降序，最多 3 条
- 命中后 `{{占位符}}` **原样输出**（如 `find {{路径}} -size +100M`），由你在输入行手改——
  关键词层不做参数抽取，误填比不填更危险
- 网络受限时可手动下载后指定 `[kb] index_url = "file:///path/tldr.zip"`，
  或临时设 `HLP_KB_URL`（优先于配置）；下载缓存留在 `~/.cache/hlp/tldr.zip`
- kb 层故障（未建库/语料空/DB 错）静默降级到 LLM，`--verbose` 会注记 `source=kb kb_score=0.xx`
- 已用官方 `tldr.zip`（20.33 MB）实测：导入 **8023 页 / 35116 条示例、跳过 0**，
  `kb-status` 计数一致；该规模下 release 版 `--one-line` kb 命中约 **55~58 ms**
  （debug 版约 310 ms）。批量打分复用意图集合且不逐行查 `kb_pages`，
  评分结果与单条打分基准逐位一致

> 关键词召回不理解语义：中文意图「列出大文件」在真实语料中会按字面命中 `ls` 页
> （`kb_score=0.78`），而不是 `find -size`。想更准，请在意图里带上命令名，
> 如 `find 大于 100M 的文件`。

> **升级提示**：v0.4 把 daemon 协议升到 v2（新增 Explain）。升级二进制后若旧 v0.3
> daemon 仍在跑（空闲 5 分钟才退出），`-x` 会自动回退直连、只是略慢；
> 想立刻用上新协议可执行 `pkill -f "hlp daemon"` 或等它自然退出。

### 解释模式 `-x`（v0.4）

按顶层 `|` 拆段（引号内与 `\|` 转义的竖线不拆），逐段给出一句话解释：

```bash
$ hlp -x "find . -name '*.jpg' | wc -l"
find . -name '*.jpg' | wc -l

  find . -name '*.jpg'  递归查找 jpg 文件
  wc -l                统计结果行数
```

- 默认经 daemon v2 一次性 LLM 综合（全部段 + kb 素材一次喂入），失败回退直连，
  再失败回退本地知识库标注；有 kb 页的段即时有解，无页的段显示"（无本地注解）"
- `hlp -x "<命令>" --offline`：只用本地语料标注，<100ms、零 LLM 请求
- `hlp -x "<命令>" --json`：输出结构化 `Explanation`（segments/lang/source），供机器消费
- `hlp -x "<命令>" --verbose`：stderr 打印 `source=local|llm|mixed lang=… elapsed=…ms`
- 解释语言跟随 `[explain] language`（auto/zh/en），auto 优先看输入里的中文，其次看 `LC_ALL`/`LANG`

### Dry-run（v0.4）

把命令变换成**只读预览**并执行预览命令，**永不执行原命令**：

```bash
$ hlp --dry-run "rm -rf /tmp/testdata"
预览（未执行原命令，列出将被删除/移除的对象）：ls -ld -- -rf /tmp/testdata
drwxrwxr-x 3 user user 4096 Nov 4 20:00 /tmp/testdata
```

内置变换表：`rm`/`rmdir` → `ls -ld -- 原参数`；`rsync` / `git clean` → 追加 `-n`；
`mv`/`cp` → `ls -ld -- 源参数`；`dd`/`mkfs*`/`shutdown`/`reboot` 永不预览；
含管道或 `; & \` $ < >` 等字符的命令一律拒绝。预览走 argv 直执（不经 shell、不展开 glob），
输出各截断 4KB、5s 超时强杀；不支持时 stdout 空、stderr 一行原因、退出 0。
**glob 不展开**是已知语义差距：`rm -rf *.bak` 的预览只显示字面量。

### TUI 卡片（v0.4）

```bash
hlp --tui "把 jpg 缩到 800 宽"   # 或裸 hlp（TTY 下等价）
```

卡片顶部是当前选中命令，中部逐段解释，底部快捷键行；候选分组显示
`[知识库]`（按 score 降序）在前、`[LLM]` 在后，两组不混排打分。

| 键 | 动作 |
|---|---|
| `A` | 轮询候选 |
| `R` | Refine：拼装原意图 + 当前命令 + 修正要求 + 已出现候选，重新生成 |
| `D` | Dry-run 预览（复用上面的只读变换） |
| `X` | 重新获取逐段解释 |
| `C` | OSC52 复制选中命令（写 `/dev/tty`，不污染 stdout） |
| `E` | 执行：先过危险熔断门（fatal 需完整重输命令原文），确认后才 `sh -c` |
| `Q` / `Esc` | 退出 / 回到卡片 |

TUI 只在真 TTY 启动；非 TTY（管道、CI）退出码 6 并提示改用 `--one-line`。
ratatui/crossterm 只在 TUI 入口初始化，`--one-line` / `--check` 的冷启动不受影响。
`E` 的熔断门与 `hlp --check` 同源，并加载同一份 `danger.toml` 信任表：
`hlp --trust` 授权过的模式在卡片里按 Clean 处理，需要确认的是 `y/N` 而非警告确认。

### 用法（引擎）

```bash
# 翻译自然语言意图（stdout 只输出一条可直接执行的命令）
hlp --one-line "列出当前目录下超过100MB的文件"

# 不加引号的多词意图同样支持
hlp 把 当前 目录 文件 列出来

# 查看全部候选（每行一条，第一条为推荐）
hlp --alt "列出大文件"

# 元信息（provider/模型/候选数/耗时）走 stderr，stdout 保持纯净
hlp --verbose "列出大文件"

# 流式：每行一个全量快照（shell 模板内部使用；末行 = 最终 top1）
hlp --stream "列出大文件"

# 解释 / 只读预览 / 卡片 / 语料维护（v0.4）
hlp -x "find . -name '*.jpg' | wc -l" --offline
hlp --dry-run "rm -rf /tmp/testdata"
hlp --tui "把 jpg 缩到 800 宽"
hlp kb-update && hlp kb-status

# 用于命令替换：退出码 0 时 $() 拿到的永远只是一条命令
echo "将执行：$(hlp --one-line "列出大文件")"
```

### 输出协议与退出码

- **stdout**：有且仅有一行——top1 命令原文（`--alt` 时每行一条候选；`--stream` 时
  每行一个快照，末行 = top1）；`-x` 是阅读型输出（命令 + 空行 + 逐段解释）；
  `--dry-run` 是预览文本；`--check` 为 `key=value`；`init` 为脚本全文；
  `--trust` / `kb-update` 为空
- **stderr**：备选提示、`--verbose` 元信息（含 `source=cache|kb|llm`）、进度/统计、
  错误信息与行动提示

| 退出码 | 含义 |
|---|---|
| 0 | 成功（含 `--check` 的 clean / info、命中信任） |
| 2 | 用法错误（参数非法、互斥冲突、非法 shell 名） |
| 3 | 配置类错误（config.toml / danger.toml 无效、读写失败） |
| 4 | LLM 服务不可达 / HTTP 错误 |
| 5 | 无可用候选 |
| 6 | `--clear` / `cache-learn` 缓存故障、`kb-update` 失败、`--tui` 在非 TTY |
| 10 | `--check` 命中 warning |
| 11 | `--check` 命中 fatal |
| 12 | `--trust` 的 pattern-id 不在内置规则表中 |

### 配置

`~/.config/hlp/config.toml`（不存在则使用默认值）：

```toml
provider = "ollama"

[ollama]
base_url = "http://127.0.0.1:11434"  # 本地 ollama 地址
model = "qwen2.5-coder:3b"           # 生成模型（可降为 1.5b 提速）
timeout_ms = 60000                   # 请求超时（含模型冷加载）
max_candidates = 3                   # 候选上限（1~10）
keep_alive = "5m"                    # ollama 侧模型保温时长

[daemon]                              # v0.3
idle_secs = 300                       # 空闲退出秒数（10~3600）
socket_path = ""                      # 留空 = $XDG_RUNTIME_DIR/hlp.sock 或 /tmp/hlp-{uid}.sock

[cache]                               # v0.3
enabled = true                        # 关掉则全部走 LLM（学习也停）
exempt_dirs = []                      # 豁免目录（不查缓存、不学习），如 ["/tmp", "~/.secrets"]

[kb]                                  # v0.4
enabled = true                        # 第 2 层开关
min_score = 0.6                       # 命中阈值（0.0..=1.0）
index_url = ""                        # 空 = 官方 tldr.zip；可设 http(s)/file URL

[explain]                             # v0.4
language = "auto"                     # auto | zh | en

[tui]                                 # v0.4
max_candidates = 3                    # TUI 生成请求的候选数（1..=8）
```

### 已知局限

- 危险检测**不做引号感知**（v0.2 设计 §9 风险 1）：`echo "curl x | sh"` 会被判为
  `pipe_to_shell`。缓解手段是 warning/info 不阻塞 + 信任机制；引号分词延后。
- zsh 的 `read` 在 ZLE widget 内的刷新行为随终端而异（设计 §9 风险 2）；
  若提示不刷新，降级方案见设计文档，需真机确认后再改。
- bash 无 preexec 钩子，学习回流用 DEBUG trap 模拟：**每条命令执行前都会触发一次
  采集函数**（有三重门控，实际开销 <1ms），但在 bash 下与其他 DEBUG trap 工具
  （如 set -x 调试器）可能互相干扰；介意可去掉 `hlp init bash` 输出末尾的 trap 行。
- 缓存前缀匹配只取**最长前缀**，不做语义相似度：意图差异大时不会误命中，
  但相近意图也不共享缓存。kb 层同样是关键词召回（不做向量检索），
  阈值依赖 tldr 中文页覆盖率，属经验参数。
- dry-run 的 **glob 不展开**：`rm -rf *.bak` 的预览只显示字面量 `*.bak`，
  预览集 ≠ 实际删除集；安全上宁可预览能力受限，也不开 shell 注入面。
- `hlp --tui` 的卡片渲染与 OSC52 复制依赖真终端能力，M-15/16 需在 zsh/bash/fish
  真终端各做一次人工验收；沙箱/CI 环境（非 TTY）会以退出码 6 明确拒绝。
- `kb-update` 联网下载 20MB 级语料，慢网络可手动下载后用 `file://` 指向本地 zip；
  上游目录结构若漂移（跳过页 >50%）会整体失败回滚，不会静默导入空库。
  官方 zip 的下载/导入已实测通过（M-17），但 M-15/16 的卡片渲染与 OSC52 复制
  仍需真终端人工验收，沙箱/CI 无法覆盖。

### 开发

```bash
cargo build
cargo test                # 151 个单元/集成测试（mock ollama + 内嵌 fixture zip，无需真 ollama）
cargo clippy --all-targets
cargo test -- --ignored   # T-I-01 真实 ollama 端到端验收（需本机 ollama 在线）
```

## 路线

| 版本 | 内容 | 状态 |
|---|---|---|
| v0.1 | 引擎：参数 → Provider(ollama) → stdout 单行命令；Provider trait | ✅ |
| v0.2 | zsh / bash / fish 集成 + `//` 前缀 + Ctrl+G 替换；危险命令熔断 + 可记忆信任 | ✅ |
| v0.3 | daemon + unix socket + 流式替换；历史缓存（采纳/修正回流、--clear、目录豁免） | ✅ |
| v0.4 | tldr 知识库；`hlp -x` 解释模式；Dry-run；TUI 卡片 | ✅ |

## License

MIT
