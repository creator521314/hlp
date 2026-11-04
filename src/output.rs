use std::io::Write;

use crate::cli::CliArgs;
use crate::engine::EngineOutcome;

/// §4.1 纯净输出协议的唯一落地处（设计文档 §2.7 / §3.3）。
///
/// 硬约束：
/// - stdout 默认有且仅有一行（top1 命令原文 + \n），无装饰、无 ANSI；
///   `--alt` 模式下每行一条候选。
/// - 备选数量提示、--verbose 元信息、一切错误信息走 stderr。
/// - 任何模块禁止绕过本模块直接向 stdout 写入。
pub fn emit(outcome: &EngineOutcome, args: &CliArgs) {
    emit_inner(outcome, args, true);
}

/// v0.3 `--stream` 的收尾：快照行已由 main 逐行写出（每行一个全量快照），
/// 这里只在"最终 top1 与最后快照不同"时补一行 top1，保证**最后一行 = 最终 top1**
/// （§3.3：缓存命中时恰好一行）。stderr 协议与 emit 一致。
pub fn emit_stream(outcome: &EngineOutcome, args: &CliArgs, last_snapshot: Option<&str>) {
    let print_top = match last_snapshot {
        Some(s) => s != outcome.top1,
        None => true,
    };
    emit_inner(outcome, args, print_top);
}

fn emit_inner(outcome: &EngineOutcome, args: &CliArgs, print_top1: bool) {
    if args.alt {
        let mut buf = String::new();
        buf.push_str(&outcome.top1);
        buf.push('\n');
        for alt in &outcome.alternatives {
            buf.push_str(alt);
            buf.push('\n');
        }
        print_pure(&buf);
    } else {
        if print_top1 {
            print_pure(&format!("{}\n", outcome.top1));
        }
        if !outcome.alternatives.is_empty() {
            eprintln!(
                "还有 {} 个备选（hlp --alt 查看）",
                outcome.alternatives.len()
            );
        }
    }
    if args.verbose {
        eprint!(
            "provider={} model={} candidates={} elapsed={}ms source={}",
            outcome.provider_name,
            outcome.model,
            outcome.alternatives.len() + usize::from(print_top1),
            outcome.elapsed.as_millis(),
            outcome.source.as_str(),
        );
        if outcome.source == crate::daemon::protocol::Source::Cache {
            if let Some(kind) = outcome.match_kind {
                eprint!(" match={}", kind.as_str());
            }
        }
        if let Some(score) = outcome.kb_score {
            eprint!(" kb_score={score:.2}");
        }
        eprintln!();
    }
}

/// 向 stdout 写入纯文本。写失败（如 EPIPE，下游 `head` 截断属正常用法）
/// → 静默退出码 0，不报错。
fn print_pure(s: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let ok = lock.write_all(s.as_bytes()).and_then(|_| lock.flush()).is_ok();
    if !ok {
        std::process::exit(0);
    }
}
