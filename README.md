# hlp

自然语言 → shell 命令翻译器。意图进，命令出，附逐段解释。

```bash
$ hlp 把当前目录下所有超过100MB的文件按大小排序
find . -type f -size +100M -exec du -h {} + | sort -rh

$ hlp -x "awk '{print $2}' access.log | sort | uniq -c | sort -rn"
```

在 zsh 中以 `// ` 前缀 + Ctrl+G 直接原地替换输入行：

```
$ // 把所有超过100MB的文件按大小列出来
      ↓ Ctrl+G
$ find . -type f -size +100M -exec du -h {} + | sort -rh
```

- 纯本地：ollama 小模型 + tldr 知识库 + 历史缓存三层响应
- 安全：危险命令熔断、Dry-run 预览、永不自动执行
- 学习型：逐段解释 + 修正回流缓存

详细设计见 [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)。

## License

MIT
