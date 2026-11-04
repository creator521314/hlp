//! hlp —— 自然语言 → shell 命令翻译器：意图进，命令出，附逐段解释。
//!
//! v0.1 引擎：命令行参数 → Provider(ollama) → stdout 单行命令（--one-line）。
//! v0.2 纯本地路径：`--check`（危险检测）、`--trust`（写信任表）、`init <shell>`。
//! v0.3：daemon-first 生成（`--stream` 流式）+ 历史缓存（`--clear` / `cache-learn`）。
//! 设计文档：docs/design/v0.1-engine.md、v0.2-shell-integration.md、v0.3-daemon-cache.md。

mod cache;
mod cli;
mod config;
mod danger;
mod daemon;
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
        Invocation::Clear => clear_cache(),
        Invocation::CacheLearn {
            intent,
            command,
            cwd,
            source,
        } => cache_learn(&intent, &command, cwd.as_deref(), &source),
        Invocation::Daemon => daemon_mode(),
    }
}

/// 生成主形态：--one-line 单行纯净；--stream 逐快照行（最后一行 = 最终 top1）。
/// stdout 纯净协议与退出码集（0/2/3/4/5）不变。
fn generate(args: CliArgs) {
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    let mut engine = Engine::from_config(cfg).unwrap_or_else(|e| fail(e));
    if args.stream {
        let mut last_snapshot: Option<String> = None;
        let outcome = engine
            .run_stream(&args.intent, &mut |snap| {
                write_stdout(&format!("{snap}\n"));
                last_snapshot = Some(snap.to_string());
            })
            .unwrap_or_else(|e| fail(e));
        output::emit_stream(&outcome, &args, last_snapshot.as_deref());
    } else {
        let outcome = engine.run(&args).unwrap_or_else(|e| fail(e));
        output::emit(&outcome, &args);
    }
}

/// `hlp --check`：纯本地判定，stdout 为固定的 key=value 协议（v0.2 设计 §3.2）。
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

/// `hlp --trust`：danger.toml 的唯一写者；成功时 stdout 为空。
fn trust(pattern: &str, example: &str) {
    danger::trust::add_trust(pattern, example).unwrap_or_else(|e| fail(e));
}

/// `hlp init`：stdout 只有脚本全文，供 `eval` / `source` 消费。
fn init(shell: integrations::Shell) {
    write_stdout(integrations::script(shell));
}

/// `hlp --clear`（v0.3 §8）：daemon 在则经 socket，不在则直连 DB；
/// 失败 → CacheUnavailable（退出码 6）。stdout 为空。
fn clear_cache() {
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    if daemon::client::ping(&cfg) {
        daemon::client::clear(&cfg).unwrap_or_else(|e| fail(e));
    } else {
        let mut c = cache::Cache::from_config(&cfg).unwrap_or_else(|e| fail(e));
        c.clear().unwrap_or_else(|e| fail(e));
    }
}

/// `hlp cache-learn`（v0.3 §4.5）：学习回流。本地操作、不走 LLM、不拉起 daemon
/// （daemon 恰好在运行则经 socket 转发）。豁免目录命中 = 静默退出 0。
fn cache_learn(intent: &str, command: &str, cwd: Option<&std::path::Path>, source: &str) {
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    let cwd = cwd
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    let source = cache::store::Source::parse(source)
        .unwrap_or(cache::store::Source::Adopted); // value_parser 已限定，防御
    if daemon::client::ping(&cfg) {
        daemon::client::learn(&cfg, intent, command, source, &cwd).unwrap_or_else(|e| fail(e));
    } else {
        let mut c = cache::Cache::from_config(&cfg).unwrap_or_else(|e| fail(e));
        c.learn_strict(intent, command, source, &cwd)
            .unwrap_or_else(|e| fail(e));
    }
}

/// `hlp daemon`（内部子命令）：主循环。已有存活实例 → 让位退出 0（非错误）。
fn daemon_mode() {
    let cfg = config::load().unwrap_or_else(|e| fail(e));
    if daemon::client::ping(&cfg) {
        return; // 让位（遗留风险 #5）：exit 0
    }
    match daemon::server::serve(&cfg) {
        Ok(_) => {}            // 空闲退出 / 让位：都是成功
        Err(e) => fail(e),
    }
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
