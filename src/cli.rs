use std::path::PathBuf;

use clap::{Arg, ArgAction, Command};

use crate::integrations::Shell;

/// 生成模式（v0.1 契约 + v0.3 `--stream`）的参数。
pub struct CliArgs {
    /// 位置参数多词以空格拼接后的完整意图，原样保留、不做改写
    pub intent: String,
    /// --one-line：单行纯净输出。缺省即此行为；显式传 flag 是为了让
    /// shell 集成侧语义明确。
    #[allow(dead_code)]
    pub one_line: bool,
    /// --alt：展开全部候选，每行一条到 stdout
    pub alt: bool,
    /// --verbose：元信息（provider / 模型 / 候选数 / 耗时 / 来源）输出到 stderr
    pub verbose: bool,
    /// --stream（v0.3）：每行一个全量快照，最后一行为最终 top1。
    /// 与 --one-line 语义同轨（走 daemon-first），与 --alt 互斥。
    pub stream: bool,
}

/// 一次调用的顶层模式。
/// 本地路径（Check/Trust/Init/Clear/CacheLearn/Daemon）均不经过 LLM。
pub enum Invocation {
    /// 意图 → 缓存/daemon/Provider → stdout 命令（--one-line 与 --stream 都在此路）
    Generate(CliArgs),
    /// 本地危险检测，`key=value` 协议 + 退出码 0/10/11
    Check(String),
    /// 写入 danger.toml 信任列表
    Trust { pattern: String, example: String },
    /// 输出 shell 集成脚本
    Init(Shell),
    /// 清空历史缓存（v0.3）
    Clear,
    /// 学习回流（v0.3，preexec 钩子调用；本地操作不走 LLM、不拉起 daemon）
    CacheLearn {
        intent: String,
        command: String,
        cwd: Option<PathBuf>,
        source: String,
    },
    /// daemon 主循环（内部子命令，一般由按需拉起触发）
    Daemon,
}

fn build_command() -> Command {
    Command::new("hlp")
        .version(env!("CARGO_PKG_VERSION"))
        .about("自然语言 → shell 命令翻译器：意图进，命令出")
        // 子命令出现时位置参数（意图）不再必需
        .subcommand_negates_reqs(true)
        // 子命令与 flags 天然互斥（`hlp init zsh --verbose` → 用法错误）
        .args_conflicts_with_subcommands(true)
        .subcommand(
            Command::new("init")
                .about("输出指定 shell 的集成脚本：eval \"$(hlp init zsh)\"")
                .arg(
                    Arg::new("shell")
                        .help("目标 shell（zsh / bash / fish）")
                        .required(true)
                        .value_parser(["zsh", "bash", "fish"]),
                ),
        )
        .subcommand(
            Command::new("daemon")
                .about("内部：daemon 主循环（空闲自动退出，一般无需手动调用）"),
        )
        .subcommand(
            Command::new("cache-learn")
                .about("学习回流：把采纳/修正的命令写入历史缓存（preexec 钩子调用）")
                .arg(
                    Arg::new("intent")
                        .long("intent")
                        .value_name("INTENT")
                        .required(true)
                        .num_args(1)
                        .help("当时的意图原文"),
                )
                .arg(
                    Arg::new("command")
                        .long("command")
                        .value_name("COMMAND")
                        .required(true)
                        .num_args(1)
                        .help("实际执行的命令"),
                )
                .arg(
                    Arg::new("cwd")
                        .long("cwd")
                        .value_name("PATH")
                        .num_args(1)
                        .help("触发目录（缺省 = 当前目录；命中豁免列表则不记录）"),
                )
                .arg(
                    Arg::new("source")
                        .long("source")
                        .value_name("SOURCE")
                        .num_args(1)
                        .value_parser(["adopted", "correction"])
                        .default_value("adopted")
                        .help("学习来源：adopted = 直接采纳，correction = 人工修正"),
                ),
        )
        .arg(
            Arg::new("intent")
                .help("自然语言意图（多词自动以空格拼接）")
                .num_args(1..)
                .trailing_var_arg(true)
                .required_unless_present_any(["check", "trust", "clear"]),
        )
        .arg(
            Arg::new("one-line")
                .long("one-line")
                .help("单行纯净输出：stdout 仅输出 top1 命令（v0.1 默认行为）")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("alt")
                .long("alt")
                .help("展开全部候选，每行一条（第一条为推荐）")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .help("元信息（provider/模型/候选数/耗时/来源）输出到 stderr")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("stream")
                .long("stream")
                .help("流式输出：stdout 每行一个全量快照，最后一行为最终 top1")
                .conflicts_with("alt")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("clear")
                .long("clear")
                .help("清空历史缓存（danger.toml 不受影响）")
                .conflicts_with_all(["one-line", "alt", "verbose", "stream", "check", "trust"])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("check")
                .long("check")
                .value_name("COMMAND")
                .num_args(1)
                .conflicts_with_all(["one-line", "alt", "verbose", "stream"])
                .help("本地危险检测（不走 LLM）：stdout 输出 key=value，退出码 0/10/11"),
        )
        .arg(
            Arg::new("trust")
                .long("trust")
                .value_name("PATTERN_ID")
                .num_args(1)
                .conflicts_with_all(["one-line", "alt", "verbose", "stream", "check"])
                .help("把危险模式 ID 写入 danger.toml 信任列表（以后不再提示）"),
        )
        .arg(
            Arg::new("example")
                .long("example")
                .value_name("COMMAND")
                .num_args(1)
                .requires("trust")
                .help("配合 --trust：触发时的样例命令，供人工审查"),
        )
}

/// 解析进程参数。用法错误返回 clap::Error，由调用方 `e.exit()` 统一处理
/// （clap 打印用法说明到 stderr，退出码 2）。
pub fn parse() -> Result<Invocation, clap::Error> {
    let mut cmd = build_command();
    let matches = cmd.clone().try_get_matches_from(std::env::args_os())?;

    if let Some((name, sub)) = matches.subcommand() {
        match name {
            "init" => {
                let shell_name = sub.get_one::<String>("shell").expect("shell 为必填参数");
                let shell = Shell::from_arg(shell_name).expect("value_parser 已限定取值");
                return Ok(Invocation::Init(shell));
            }
            "daemon" => return Ok(Invocation::Daemon),
            "cache-learn" => {
                return Ok(Invocation::CacheLearn {
                    intent: sub.get_one::<String>("intent").expect("required").clone(),
                    command: sub.get_one::<String>("command").expect("required").clone(),
                    cwd: sub.get_one::<String>("cwd").map(PathBuf::from),
                    source: sub.get_one::<String>("source").expect("有默认值").clone(),
                });
            }
            _ => unreachable!("未注册的子命令"),
        }
    }

    let intent: Vec<String> = matches
        .get_many::<String>("intent")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();

    if matches.get_flag("clear") {
        if !intent.is_empty() {
            return Err(conflict(&mut cmd, "--clear 不能与位置参数意图同时使用"));
        }
        return Ok(Invocation::Clear);
    }

    if let Some(command) = matches.get_one::<String>("check") {
        if !intent.is_empty() {
            return Err(conflict(&mut cmd, "--check 不能与位置参数意图同时使用"));
        }
        return Ok(Invocation::Check(command.clone()));
    }

    if let Some(pattern) = matches.get_one::<String>("trust") {
        if !intent.is_empty() {
            return Err(conflict(&mut cmd, "--trust 不能与位置参数意图同时使用"));
        }
        let example = matches
            .get_one::<String>("example")
            .cloned()
            .unwrap_or_default();
        return Ok(Invocation::Trust {
            pattern: pattern.clone(),
            example,
        });
    }

    if intent.is_empty() {
        return Err(cmd.error(
            clap::error::ErrorKind::MissingRequiredArgument,
            "缺少意图参数（或使用 --check / --trust / --clear / init）",
        ));
    }

    Ok(Invocation::Generate(CliArgs {
        intent: intent.join(" "),
        one_line: matches.get_flag("one-line"),
        alt: matches.get_flag("alt"),
        verbose: matches.get_flag("verbose"),
        stream: matches.get_flag("stream"),
    }))
}

fn conflict(cmd: &mut Command, message: &str) -> clap::Error {
    cmd.error(clap::error::ErrorKind::ArgumentConflict, message)
}
