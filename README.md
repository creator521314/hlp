# hlp

自然语言 → shell 命令翻译器。意图进，命令出，附逐段解释。

```bash
$ hlp 把当前目录下所有超过100MB的文件按大小排序
find . -type f -size +100M -exec du -h {} + | sort -rh
```

规划中的 zsh 集成（v0.2）：以 `// ` 前缀 + Ctrl+G 直接原地替换输入行：

```
$ // 把所有超过100MB的文件按大小列出来
      ↓ Ctrl+G
$ find . -type f -size +100M -exec du -h {} + | sort -rh
```

- 纯本地：ollama 小模型 + tldr 知识库 + 历史缓存三层响应（缓存与知识库于 v0.3/v0.4 交付）
- 安全：危险命令熔断、Dry-run 预览、永不自动执行（熔断于 v0.2 交付）
- 学习型：逐段解释 + 修正回流缓存（v0.4 交付）

详细需求见 [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)，v0.1 引擎设计见 [docs/design/v0.1-engine.md](docs/design/v0.1-engine.md)。

## 当前状态：v0.1（引擎）

已实现：命令行参数 → Provider(ollama) → stdout 单行命令。

### 安装与前置

- Rust 工具链（stable）
- 本地 [ollama](https://ollama.com) 已运行，且已拉取默认模型：
  ```bash
  ollama pull qwen2.5-coder:3b
  ```

### 用法

```bash
# 翻译自然语言意图（stdout 只输出一条可直接执行的命令）
hlp --one-line "列出当前目录下超过100MB的文件"

# 不加引号的多词意图同样支持
hlp 把 当前 目录 文件 列出来

# 查看全部候选（每行一条，第一条为推荐）
hlp --alt "列出大文件"

# 元信息（provider/模型/候选数/耗时）走 stderr，stdout 保持纯净
hlp --verbose "列出大文件"

# 用于命令替换：退出码 0 时 $() 拿到的永远只是一条命令
echo "将执行：$(hlp --one-line "列出大文件")"
```

### 输出协议

- **stdout**：有且仅有一行——top1 命令原文（`--alt` 时每行一条候选）
- **stderr**：备选数量提示、`--verbose` 元信息、错误信息与行动提示
- **退出码**：`0` 成功；`2` 用法错误；`3` 配置错误；`4` LLM 服务不可达/HTTP 错误；`5` 无可用候选

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
```

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
| v0.2 | zsh / bash / fish 集成 + `//` 前缀 + Ctrl+G 替换；危险命令熔断 | |
| v0.3 | daemon + unix socket + 流式替换；历史缓存 | |
| v0.4 | tldr 知识库；`hlp -x` 解释模式；Dry-run；TUI 卡片 | |

## License

MIT
