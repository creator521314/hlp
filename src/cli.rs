use clap::{Arg, ArgAction, Command};

/// 解析后的命令行参数（契约见 docs/design/v0.1-engine.md §2.1）。
pub struct CliArgs {
    /// 位置参数多词以空格拼接后的完整意图，原样保留、不做改写
    pub intent: String,
    /// --one-line：单行纯净输出。v0.1 中这是唯一输出形态，缺省即此行为；
    /// 显式传 flag 是为了让 shell 集成侧语义明确。字段为 v0.2 shell 集成
    /// 契约预留，暂未在 output 分支中读取。
    #[allow(dead_code)]
    pub one_line: bool,
    /// --alt：展开全部候选，每行一条到 stdout
    pub alt: bool,
    /// --verbose：元信息（provider / 模型 / 候选数 / 耗时）输出到 stderr
    pub verbose: bool,
}

fn build_command() -> Command {
    Command::new("hlp")
        .version(env!("CARGO_PKG_VERSION"))
        .about("自然语言 → shell 命令翻译器：意图进，命令出")
        .arg(
            Arg::new("intent")
                .help("自然语言意图（多词自动以空格拼接）")
                .num_args(1..)
                .trailing_var_arg(true)
                .required(true),
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
                .help("元信息（provider/模型/候选数/耗时）输出到 stderr")
                .action(ArgAction::SetTrue),
        )
}

/// 解析进程参数。用法错误返回 clap::Error，由调用方 `e.exit()` 统一处理
/// （clap 打印用法说明到 stderr，退出码 2）。
pub fn parse() -> Result<CliArgs, clap::Error> {
    let matches = build_command().try_get_matches_from(std::env::args_os())?;
    let intent: Vec<String> = matches
        .get_many::<String>("intent")
        .expect("intent 为必填位置参数")
        .cloned()
        .collect();
    Ok(CliArgs {
        intent: intent.join(" "),
        one_line: matches.get_flag("one-line"),
        alt: matches.get_flag("alt"),
        verbose: matches.get_flag("verbose"),
    })
}
