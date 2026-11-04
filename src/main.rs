//! hlp —— 自然语言 → shell 命令翻译器：意图进，命令出，附逐段解释。
//!
//! v0.1 引擎：命令行参数 → Provider(ollama) → stdout 单行命令（--one-line）。
//! 设计文档：docs/design/v0.1-engine.md；需求文档：docs/REQUIREMENTS.md。

mod cli;
mod config;
mod engine;
mod error;
mod output;
mod prompt;
mod provider;

use engine::Engine;
use error::HlpError;

fn main() {
    // 用法错误：clap 打印用法说明到 stderr，退出码 2
    let args = cli::parse().unwrap_or_else(|e| e.exit());
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    let engine = Engine::from_config(cfg).unwrap_or_else(|e| fail(e));
    let outcome = engine.run(&args).unwrap_or_else(|e| fail(e));
    output::emit(&outcome, &args);
}

/// 错误统一出口：stderr 下一行打印（`hlp: <错误>` + 提示），按分类退出。
fn fail(e: HlpError) -> ! {
    eprintln!("hlp: {e}");
    if let Some(hint) = e.hint() {
        eprintln!("提示：{hint}");
    }
    std::process::exit(e.exit_code());
}
