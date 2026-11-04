//! hlp —— 自然语言 → shell 命令翻译器：意图进，命令出，附逐段解释。
//!
//! v0.1 引擎：命令行参数 → Provider(ollama) → stdout 单行命令（--one-line）。
//! v0.2 新增三条纯本地路径：`--check`（危险检测）、`--trust`（写信任表）、
//! `init <shell>`（输出集成脚本），全部不经过 LLM。
//! 设计文档：docs/design/v0.1-engine.md、docs/design/v0.2-shell-integration.md。

mod cli;
mod config;
mod danger;
mod engine;
mod error;
mod integrations;
mod output;
mod prompt;
mod provider;

use cli::{CliArgs, Invocation};
use engine::Engine;
use error::HlpError;

fn main() {
    // 用法错误：clap 打印用法说明到 stderr，退出码 2
    match cli::parse().unwrap_or_else(|e| e.exit()) {
        Invocation::Generate(args) => generate(args),
        Invocation::Check(command) => check(&command),
        Invocation::Trust { pattern, example } => trust(&pattern, &example),
        Invocation::Init(shell) => init(shell),
    }
}

/// v0.1 主形态（行为与退出码 0/2/3/4/5 完全不变）。
fn generate(args: CliArgs) {
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    let engine = Engine::from_config(cfg).unwrap_or_else(|e| fail(e));
    let outcome = engine.run(&args).unwrap_or_else(|e| fail(e));
    output::emit(&outcome, &args);
}

/// `hlp --check`：纯本地判定，stdout 为固定的 key=value 协议（设计 §3.2）。
///
/// 协议漂移会让三个 shell 模板的 `sed -n 's/^level=//p'` 静默失效，
/// 因此：键序固定、值内不含换行、无空行、无 BOM、无 ANSI。
fn check(command: &str) {
    let trust = danger::trust::load().unwrap_or_else(|e| fail(e));
    let verdict = danger::check(command, &trust);

    let mut buf = format!("level={}\n", verdict.level.as_str());
    if let Some(pattern) = verdict.pattern {
        buf.push_str(&format!("pattern={pattern}\n"));
    }
    if verdict.trusted {
        buf.push_str("trusted=true\n");
    }
    if !verdict.trusted && verdict.level != danger::DangerLevel::Clean {
        buf.push_str(&format!("reason={}\n", verdict.reason));
    }
    write_stdout(&buf);
    std::process::exit(verdict.level.exit_code());
}

/// `hlp --trust`：danger.toml 的唯一写者；成功时 stdout 为空（设计 §3.5）。
fn trust(pattern: &str, example: &str) {
    danger::trust::add_trust(pattern, example).unwrap_or_else(|e| fail(e));
}

/// `hlp init`：stdout 只有脚本全文，供 `eval` / `source` 消费。
fn init(shell: integrations::Shell) {
    write_stdout(integrations::script(shell));
}

/// 向 stdout 写纯文本。写失败（如 EPIPE，`hlp init zsh | head -1` 属正常用法）
/// → 静默退出码 0，不报错。
fn write_stdout(s: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if lock
        .write_all(s.as_bytes())
        .and_then(|_| lock.flush())
        .is_err()
    {
        std::process::exit(0);
    }
}

/// 错误统一出口：stderr 下一行打印（`hlp: <错误>` + 提示），按分类退出。
fn fail(e: HlpError) -> ! {
    eprintln!("hlp: {e}");
    if let Some(hint) = e.hint() {
        eprintln!("提示：{hint}");
    }
    std::process::exit(e.exit_code());
}
