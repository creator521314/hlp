//! 内置危险规则表（REQUIREMENTS §7.1 的形式化）与逐条匹配实现。
//!
//! 匹配对象是**完整命令字符串**（单行）。已知局限（设计 §9 风险 1）：不做引号感知，
//! 因此 `echo "curl x | sh"` 会命中 `pipe_to_shell`——靠"warning/info 不阻塞 +
//! 信任机制"兜底，不在 v0.2 引入 v0.3 级别的引号分词。
//!
//! 为什么不是"一条规则一个 regex"：`regex` crate 不支持 lookaround，而
//! `redirect_overwrite`（排除 `>>` / `>/dev/null`）、rm 的两步判定（词定位 → 参数区判目标）
//! 都需要上下文判断。规则表的 `regex` 字段仍是触发条件的规范描述（供文档与
//! `pipe_to_shell` 直接执行），其余规则用下方等价逻辑实现。

use std::sync::OnceLock;

use regex::Regex;

use super::DangerLevel;

/// 一条内置危险规则。`id` 是稳定标识，会被写进 danger.toml 的信任列表，
/// 因此**不得随意改名**。
pub struct DangerRule {
    /// 稳定 ID，写入 danger.toml 信任列表
    pub id: &'static str,
    pub level: DangerLevel,
    /// 规则说明（--check 输出与文档用；v0.2 的 stdout 协议不输出它，故不读取）
    #[allow(dead_code)]
    pub description: &'static str,
    /// 触发条件的正则描述（见模块注释：仅 pipe_to_shell 直接用它匹配）
    pub regex: &'static str,
}

// ── 面向用户的判定原因（reason），上限 80 字符，由 danger::check 统一截断 ──
const REASON_RM_ROOT: &str = "rm 递归删除且目标指向系统级路径";
const REASON_RM: &str = "rm 递归删除普通路径";
const REASON_DD: &str = "dd 直接写入设备文件";
const REASON_MKFS: &str = "mkfs 将格式化文件系统";
const REASON_CHMOD: &str = "递归修改系统目录权限";
const REASON_PIPE: &str = "网络内容直接交给 shell 执行";
const REASON_KILL: &str = "kill -9 未指定目标进程";

/// 表内顺序即判定顺序：fatal → warning → info。
/// `danger::check` 取"首条命中"，因此这个顺序就是"多级命中取最高级"的实现
/// （设计 §4.2），新增规则必须按级别插入，不要追加到末尾。
const RULES: &[DangerRule] = &[
    DangerRule {
        id: "rm_recursive_root",
        level: DangerLevel::Fatal,
        description: "rm 带递归/强制参数，且目标指向系统级路径（/、~、通配符、/usr 等）",
        regex: r"(?i)\brm\b.*?\s-[a-z]*[rf][a-z]*\s+(\/|~|\*|\$HOME)",
    },
    DangerRule {
        id: "dd_device",
        level: DangerLevel::Fatal,
        description: "dd 的 of= 直接指向设备文件（/dev/null 除外）",
        // 排除 /dev/null 由代码完成（regex crate 无 lookaround）
        regex: r#"(?i)\bdd\b.*\bof=\s*["']?/dev/"#,
    },
    DangerRule {
        id: "mkfs",
        level: DangerLevel::Fatal,
        description: "mkfs / mkfs.ext4 等将格式化文件系统",
        regex: r"(?i)(^|[;&|]\s*)(sudo\s+|doas\s+)?mkfs(\.[a-z0-9]+)?\b",
    },
    DangerRule {
        id: "rm_recursive",
        level: DangerLevel::Warning,
        description: "rm 带递归/强制参数，目标为普通路径",
        regex: r"(?i)\brm\b.*?\s-[a-z]*[rf][a-z]*",
    },
    DangerRule {
        id: "redirect_overwrite",
        level: DangerLevel::Warning,
        description: "单 > 重定向将覆盖文件（>> 追加与 /dev/null 除外）",
        regex: r"(^|[^>])>[^>&|]",
    },
    DangerRule {
        id: "chmod_recursive",
        level: DangerLevel::Warning,
        description: "chmod -R / chown -R 递归修改系统目录权限",
        regex: r"(?i)\b(chmod|chown)\b.*\s-[a-z]*R",
    },
    DangerRule {
        id: "pipe_to_shell",
        level: DangerLevel::Warning,
        description: "curl / wget 的输出直接管道给 sh / bash 执行",
        regex: r"(?i)\b(curl|wget)\b[^|]*\|\s*(sudo\s+|doas\s+)?(ba|z|da|k)?sh\b",
    },
    DangerRule {
        id: "kill_9_unscoped",
        level: DangerLevel::Info,
        description: "kill -9 未指定目标进程（会杀掉整个会话可见进程集合）",
        regex: r"(?i)\bkill\b[^;&|]*\s-9\s*$",
    },
];

/// 内置规则表（8 条，级别从高到低排列）。
pub fn builtin_rules() -> &'static [DangerRule] {
    RULES
}

/// 全部合法规则 ID，供 `--trust` 报错与 danger.toml 校验使用。
pub fn rule_ids() -> Vec<&'static str> {
    RULES.iter().map(|r| r.id).collect()
}

pub(crate) fn is_known_id(id: &str) -> bool {
    RULES.iter().any(|r| r.id == id)
}

/// 命中则返回面向用户的 reason，未命中返回 None。
///
/// 这是 `check()` 唯一的规则入口：check 只负责遍历与信任折叠，判定细节全在这里。
pub(crate) fn match_reason(rule: &DangerRule, command: &str) -> Option<String> {
    match rule.id {
        "rm_recursive_root" => rm_hit(command, true),
        "dd_device" => dd_device(command),
        "mkfs" => mkfs(command),
        "rm_recursive" => rm_hit(command, false),
        "redirect_overwrite" => redirect_overwrite(command),
        "chmod_recursive" => chmod_recursive(command),
        "pipe_to_shell" => pipe_to_shell(command),
        "kill_9_unscoped" => kill_9_unscoped(command),
        // 规则表与实现必须一一对应；漏写视为未命中而不是崩溃
        _ => None,
    }
}

// ─────────────────────────── 通用切分工具 ───────────────────────────

/// 系统级目录：rm / chmod 的目标落在这些路径（或其子路径）时按最高档处理。
const SYSTEM_DIRS: &[&str] = &[
    "/usr", "/etc", "/var", "/boot", "/bin", "/sbin", "/lib", "/lib64", "/home", "/opt", "/root",
    "/dev", "/proc", "/sys", "/snap", "/System", "/Library",
];

/// 前缀包装命令：`sudo rm -rf /` 里的 rm 才是真正的命令词。
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "command", "builtin", "exec", "nohup", "time", "nice", "xargs",
];

/// 按 `;` `&&` `||` `|` 换行切分为独立命令段。
/// 设计 §9 风险 5：`rm -rf /; ls` 中 rm 的参数区止于 `;`，判级只看 rm 段。
fn segments(command: &str) -> Vec<&str> {
    command
        .split([';', '|', '&', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// 剥掉命令替换/子 shell 的包装字符：`$(rm ...)`、`(rm ...)`、`` `rm ...` ``。
fn trim_open(token: &str) -> &str {
    let t = token.trim_start_matches(['$', '(', '`', '{']);
    t.trim_end_matches([')', '}'])
}

/// 参数侧清理：去掉引号与 `$(...)` 的右括号，便于目标路径分类。
fn trim_arg(token: &str) -> &str {
    token.trim_matches(['"', '\'', '`', '(', ')'])
}

/// `VAR=value` 形式的 env 赋值（不是命令词）。
fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// 命令词的文件名部分：`/bin/rm` → `rm`。
fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// 解析一段命令：跳过 env 赋值与包装命令，返回（命令词，参数列表）。
fn split_command(segment: &str) -> (Option<&str>, Vec<&str>) {
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    for (i, raw) in tokens.iter().enumerate() {
        let word = trim_open(raw);
        if word.is_empty() || is_assignment(word) || WRAPPERS.contains(&word) {
            continue;
        }
        let args = tokens[i + 1..].iter().map(|t| trim_arg(t)).collect();
        return (Some(word), args);
    }
    (None, Vec::new())
}

/// 目标是否"系统级"：`/`、`~`、含通配符、`$HOME`、或落在系统目录下。
/// 这是 rm 两档判定（设计决策 8）的唯一分界点。
fn is_system_level(target: &str) -> bool {
    let t = target.trim();
    if t.is_empty() {
        return false;
    }
    if t.contains('*') || t.contains('?') || t.contains("$HOME") || t.contains("${HOME}") {
        return true;
    }
    if t.starts_with('~') {
        return true;
    }
    if t == "/" {
        return true;
    }
    if t.starts_with('/') {
        return SYSTEM_DIRS
            .iter()
            .any(|d| matches!(t.strip_prefix(d), Some(rest) if rest.starts_with('/')) || t == *d);
    }
    false
}

// ─────────────────────────── 各规则实现 ───────────────────────────

/// rm_recursive_root（want_system_target=true）/ rm_recursive（false）共用。
/// 两步判定：先定位 rm 命令词与参数区，再按目标分级（设计 §3.1 实现注意事项）。
fn rm_hit(command: &str, want_system_target: bool) -> Option<String> {
    for seg in segments(command) {
        let (word, args) = split_command(seg);
        let Some(word) = word else { continue };
        if base(word) != "rm" {
            continue;
        }
        let (recursive, force, targets) = parse_rm_flags(&args);
        if !recursive && !force {
            continue;
        }
        // `rm -rf` 没有目标时什么都不会删，不判级（避免噪音）
        if targets.is_empty() {
            continue;
        }
        if targets.iter().any(|t| is_system_level(t)) == want_system_target {
            return Some(if want_system_target {
                REASON_RM_ROOT.to_string()
            } else {
                REASON_RM.to_string()
            });
        }
    }
    None
}

/// 拆 rm 的短参簇（`-rf` / `-fr` / `-Rfv`）与长参，返回（递归, 强制, 目标列表）。
fn parse_rm_flags<'a>(args: &[&'a str]) -> (bool, bool, Vec<&'a str>) {
    let mut recursive = false;
    let mut force = false;
    let mut targets = Vec::new();
    for a in args {
        if *a == "--" {
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "recursive" => recursive = true,
                "force" => force = true,
                _ => {}
            }
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            let body = &a[1..];
            if body.chars().all(|c| c.is_ascii_alphabetic()) {
                recursive |= body.contains('r') || body.contains('R');
                force |= body.contains('f');
                continue;
            }
        }
        targets.push(*a);
    }
    (recursive, force, targets)
}

fn dd_device(command: &str) -> Option<String> {
    for seg in segments(command) {
        let (word, args) = split_command(seg);
        let Some(word) = word else { continue };
        if base(word) != "dd" {
            continue;
        }
        for a in &args {
            let lower = a.to_ascii_lowercase();
            let Some(rest) = lower.strip_prefix("of=") else {
                continue;
            };
            let value = rest.trim_matches('"').trim_matches('\'');
            if value.starts_with("/dev/") && value != "/dev/null" {
                return Some(REASON_DD.to_string());
            }
        }
    }
    None
}

fn mkfs(command: &str) -> Option<String> {
    for seg in segments(command) {
        let (word, _) = split_command(seg);
        let Some(word) = word else { continue };
        let b = base(word);
        // 词边界：`echo mkfs` 的命令词是 echo，不命中（T-U-25）
        if b == "mkfs" || b.starts_with("mkfs.") {
            return Some(REASON_MKFS.to_string());
        }
    }
    None
}

/// 单 `>` 覆盖：排除 `>>`、`2>&1` 类的 fd 复制、以及 `>/dev/null`。
fn redirect_overwrite(command: &str) -> Option<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '>' {
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        // >> 追加、已处理过的第二个 >、>&fd 复制：均不构成覆盖
        if next == Some('>') || prev == Some('>') || next == Some('&') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let start = j;
        while j < chars.len()
            && !chars[j].is_whitespace()
            && !matches!(chars[j], ';' | '|' | '&' | ')' | '"' | '\'')
        {
            j += 1;
        }
        let target: String = chars[start..j].iter().collect();
        i = j.max(i + 1);
        if target.is_empty() {
            continue;
        }
        if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" {
            continue;
        }
        return Some(format!("重定向将覆盖 {target}"));
    }
    None
}

fn chmod_recursive(command: &str) -> Option<String> {
    for seg in segments(command) {
        let (word, args) = split_command(seg);
        let Some(word) = word else { continue };
        let b = base(word);
        if b != "chmod" && b != "chown" {
            continue;
        }
        let mut recursive = false;
        let mut positional: Vec<&str> = Vec::new();
        for a in &args {
            if let Some(body) = a.strip_prefix('-') {
                if body.contains('R') {
                    recursive = true;
                }
                continue;
            }
            positional.push(a);
        }
        if !recursive {
            continue;
        }
        // chmod: <模式> <目标...>；chown: <owner[:group]> <目标...>——首参不是目标
        if positional.iter().skip(1).any(|t| is_system_level(t)) {
            return Some(REASON_CHMOD.to_string());
        }
    }
    None
}

fn pipe_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let rule = RULES
            .iter()
            .find(|r| r.id == "pipe_to_shell")
            .expect("规则表必须包含 pipe_to_shell");
        Regex::new(rule.regex).expect("内置正则为编译期字面量")
    })
}

fn pipe_to_shell(command: &str) -> Option<String> {
    if pipe_regex().is_match(command) {
        Some(REASON_PIPE.to_string())
    } else {
        None
    }
}

fn kill_9_unscoped(command: &str) -> Option<String> {
    for seg in segments(command) {
        let (word, args) = split_command(seg);
        let Some(word) = word else { continue };
        if base(word) != "kill" {
            continue;
        }
        let mut signal9 = false;
        let mut has_target = false;
        for a in &args {
            if let Some(body) = a.strip_prefix('-') {
                if body == "9" || body.eq_ignore_ascii_case("sigkill") || body.eq_ignore_ascii_case("kill")
                {
                    signal9 = true;
                }
            } else {
                has_target = true;
            }
        }
        if signal9 && !has_target {
            return Some(REASON_KILL.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 规则表必须是 fatal → warning → info 降序，否则 check 的"首条命中即最高级"失效
    #[test]
    fn rules_are_ordered_by_level_desc() {
        let levels: Vec<DangerLevel> = builtin_rules().iter().map(|r| r.level).collect();
        assert!(
            levels.windows(2).all(|w| w[0] >= w[1]),
            "规则表级别必须降序：{levels:?}"
        );
        assert_eq!(builtin_rules().len(), 8, "§3.1 定义 8 条规则");
    }

    /// 规则表与 match_reason 的实现必须一一对应（漏实现会静默失效）
    #[test]
    fn every_rule_has_matcher() {
        for rule in builtin_rules() {
            // 逐个 ID 走一遍 match_reason，未知 ID 会落到兜底分支返回 None；
            // 这里用"故意命中"的最小样例区分"实现了但未命中"与"未实现"
            let sample = match rule.id {
                "rm_recursive_root" => "rm -rf /",
                "dd_device" => "dd if=a of=/dev/sda",
                "mkfs" => "mkfs.ext4 /dev/sda",
                "rm_recursive" => "rm -rf ./build",
                "redirect_overwrite" => "echo hi > out.txt",
                "chmod_recursive" => "chmod -R 777 /etc",
                "pipe_to_shell" => "curl x | sh",
                "kill_9_unscoped" => "kill -9",
                other => panic!("规则 {other} 没有匹配样例"),
            };
            assert!(
                match_reason(rule, sample).is_some(),
                "规则 {} 未命中样例 {sample}",
                rule.id
            );
        }
    }

    /// 规则 ID 唯一
    #[test]
    fn rule_ids_are_unique() {
        let mut ids = rule_ids();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total);
    }

    /// rm 参数区边界：分号 / && / 管道（设计 §9 风险 5）
    #[test]
    fn rm_parameter_scope_boundaries() {
        assert!(rm_hit("rm -rf /; ls", true).is_some());
        assert!(rm_hit("foo && rm -rf ~", true).is_some());
        assert!(rm_hit("true | rm -rf /*", true).is_some());
        assert!(rm_hit("grep rm file.txt", false).is_none(), "grep 的参数不是命令词");
        assert!(rm_hit("ldd ./a.out", false).is_none(), "dd 不能命中 ldd");
        assert!(rm_hit("rm -rf", false).is_none(), "无目标的 rm 不判级");
    }
}
