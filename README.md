# hlp

自然语言 → shell 命令翻译器。意图进，命令出，附逐段解释。

```bash
$ hlp 把当前目录下所有超过100MB的文件按大小排序
find . -type f -size +100M -exec du -h {} + | sort -rh
```

v0.2 起支持 shell 输入行原地替换：`// ` 前缀 + Ctrl+G，命令直接落回输入行，回车才执行。
v0.3 起生成过程**流式打字**（逐快照刷新输入行），命令执行后自动学习回流进历史缓存，
下次同样意图直接从缓存出（毫秒级，不再走 LLM）。

```
$ // 把所有超过100MB的文件按大小列出来
      ↓ Ctrl+G（流式：命令像打字一样逐字浮现）
$ find . -type f -size +100M -exec du -h {} + | sort -rh   ← 光标在末尾
```

- 纯本地：ollama 小模型 + 历史缓存（v0.3）+ tldr 知识库（v0.4）三层响应
- 安全：危险命令熔断、可记忆信任、永不自动执行
- 学习型：逐段解释 + 采纳/修正自动回流缓存

详细需求见 [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)，设计文档见
[docs/design/v0.1-engine.md](docs/design/v0.1-engine.md)、
[docs/design/v0.2-shell-integration.md](docs/design/v0.2-shell-integration.md)、
[docs/design/v0.3-daemon-cache.md](docs/design/v0.3-daemon-cache.md)。

## 当前状态：v0.3（daemon + 历史缓存 + 流式替换）

已实现：

- v0.1：命令行参数 → Provider(ollama) → stdout 单行命令
- v0.2：zsh / bash / fish 三种集成（`//` + Ctrl+G 原地替换、再按撤销）；
  危险命令分级检测（`hlp --check`）+ 可记忆信任（`hlp --trust` → `danger.toml`）
- v0.3：后台 daemon（unix socket，首次使用自动拉起、空闲自退出）+ SQLite 历史缓存
  （精确/前缀匹配、采纳与修正回流、`--clear` 清空、`exempt_dirs` 目录豁免）；
  shell 集成模板 v2——`hlp --stream` 流式替换 + preexec 学习回流

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

# 用于命令替换：退出码 0 时 $() 拿到的永远只是一条命令
echo "将执行：$(hlp --one-line "列出大文件")"
```

### 输出协议与退出码

- **stdout**：有且仅有一行——top1 命令原文（`--alt` 时每行一条候选；`--stream` 时
  每行一个快照，末行 = top1）；`--check` 为 `key=value`；`init` 为脚本全文；
  `--trust` 为空
- **stderr**：备选提示、`--verbose` 元信息（含 `source=cache|llm`）、错误信息与行动提示

| 退出码 | 含义 |
|---|---|
| 0 | 成功（含 `--check` 的 clean / info、命中信任） |
| 2 | 用法错误（参数非法、互斥冲突、非法 shell 名） |
| 3 | 配置类错误（config.toml / danger.toml 无效、读写失败） |
| 4 | LLM 服务不可达 / HTTP 错误 |
| 5 | 无可用候选 |
| 6 | `--clear` / `cache-learn` 显式操作缓存失败 |
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
```

### 已知局限

- 危险检测**不做引号感知**（设计 §9 风险 1）：`echo "curl x | sh"` 会被判为
  `pipe_to_shell`。缓解手段是 warning/info 不阻塞 + 信任机制；引号分词留给 v0.4。
- zsh 的 `read` 在 ZLE widget 内的刷新行为随终端而异（设计 §9 风险 2）；
  若提示不刷新，降级方案见设计文档，需真机确认后再改。
- bash 无 preexec 钩子，学习回流用 DEBUG trap 模拟：**每条命令执行前都会触发一次
  采集函数**（有三重门控，实际开销 <1ms），但在 bash 下与其他 DEBUG trap 工具
  （如 set -x 调试器）可能互相干扰；介意可去掉 `hlp init bash` 输出末尾的 trap 行。
- 缓存前缀匹配只取**最长前缀**，不做语义相似度：意图差异大时不会误命中，
  但相近意图也不共享缓存（v0.4 引入语义匹配的候选）。

### 开发

```bash
cargo build
cargo test                # 单元 + 集成测试（集成用 mock server，无需 ollama）
cargo test -- --ignored   # T-I-01 真实 ollama 端到端验收（需本机 ollama 在线）
```

## 路线

| 版本 | 内容 | 状态 |
|---|---|---|
| v0.1 | 引擎：参数 → Provider(ollama) → stdout 单行命令；Provider trait | ✅ |
| v0.2 | zsh / bash / fish 集成 + `//` 前缀 + Ctrl+G 替换；危险命令熔断 + 可记忆信任 | ✅ |
| v0.3 | daemon + unix socket + 流式替换；历史缓存（采纳/修正回流、--clear、目录豁免） | ✅ |
| v0.4 | tldr 知识库；`hlp -x` 解释模式；Dry-run；TUI 卡片 | |

## License

MIT
