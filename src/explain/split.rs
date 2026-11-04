//! 管道拆段（v0.4 设计 §2.5）——全文最重要的正确性点之一。
//!
//! 只按**顶层** `|` 拆：单双引号内不拆，`\|` 转义不拆。实现用字符状态机
//! （Normal / SingleQuote / DoubleQuote），正则切分必错（遗留风险 #1）。

/// 拆段。引号未闭合 → 按已扫描内容尽力拆（不报错）；每段 trim，空段丢弃。
pub fn split_pipeline(cmd: &str) -> Vec<String> {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        Single,
        Double,
    }
    let mut state = State::Normal;
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();

    while let Some(c) = chars.next() {
        match state {
            State::Normal => match c {
                '\\' => {
                    cur.push('\\');
                    if let Some(next) = chars.next() {
                        cur.push(next); // `\|` 原样保留，不触发拆段
                    }
                }
                '\'' => {
                    state = State::Single;
                    cur.push(c);
                }
                '"' => {
                    state = State::Double;
                    cur.push(c);
                }
                '|' => {
                    let s = cur.trim();
                    if !s.is_empty() {
                        segments.push(s.to_string());
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
            State::Single => {
                cur.push(c);
                if c == '\'' {
                    state = State::Normal;
                }
            }
            State::Double => match c {
                '\\' => {
                    cur.push('\\');
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                }
                '"' => {
                    state = State::Normal;
                    cur.push(c);
                }
                _ => cur.push(c),
            },
        }
    }
    let s = cur.trim();
    if !s.is_empty() {
        segments.push(s.to_string());
    }
    segments
}

/// shell 词法切 token（去掉引号、保留反斜杠转义的下一字符）。
/// dryrun 也复用此函数——它决定命令如何被 argv 直执（设计 §2.8）。
pub fn lex_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
                started = true;
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    } else {
                        cur.push('\\');
                    }
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                _ => {
                    cur.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// 提取段的基础命令名：跳过前导环境变量赋值（FOO=bar）与 sudo/doas，
/// 取首个有效 token（已去引号）。空段 → None。
pub fn base_command(segment: &str) -> Option<String> {
    let tokens = lex_tokens(segment);
    for tok in tokens {
        if tok.is_empty() {
            continue;
        }
        if is_env_assignment(&tok) || tok == "sudo" || tok == "doas" {
            continue;
        }
        // 含路径时取文件名（/usr/bin/find → find）
        let base = tok.rsplit('/').next().unwrap_or(&tok);
        if base.is_empty() {
            continue;
        }
        return Some(base.to_string());
    }
    None
}

fn is_env_assignment(t: &str) -> bool {
    let Some((k, _)) = t.split_once('=') else {
        return false;
    };
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-51：`\|` 转义不拆
    #[test]
    fn tu51_escaped_pipe_not_split() {
        assert_eq!(
            split_pipeline("a | b \\| c"),
            vec!["a".to_string(), "b \\| c".to_string()]
        );
    }

    /// T-U-52：引号内竖线不拆
    #[test]
    fn tu52_quoted_pipe_not_split() {
        assert_eq!(
            split_pipeline("grep \"a|b\" f | wc -l"),
            vec!["grep \"a|b\" f".to_string(), "wc -l".to_string()]
        );
        assert_eq!(
            split_pipeline("awk '{print $1|2}' f | sort"),
            vec!["awk '{print $1|2}' f".to_string(), "sort".to_string()]
        );
    }

    /// T-U-53：跳过赋值前缀与 sudo
    #[test]
    fn tu53_base_command_skips_assign_and_sudo() {
        assert_eq!(
            base_command("FOO=1 sudo rm -rf /x"),
            Some("rm".to_string())
        );
        assert_eq!(base_command("LC_ALL=C /usr/bin/find ."), Some("find".to_string()));
        assert_eq!(base_command("  "), None);
    }

    #[test]
    fn split_drops_empty_segments_and_tolerates_unclosed_quote() {
        assert_eq!(split_pipeline("a |  | b"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(split_pipeline("a |"), vec!["a".to_string()]);
        // 未闭合引号：尽力按已扫描内容拆，不 panic
        assert_eq!(split_pipeline("echo \"a | b"), vec!["echo \"a | b".to_string()]);
    }

    #[test]
    fn lex_tokens_handles_quotes_and_escapes() {
        assert_eq!(
            lex_tokens("awk '{print $2}' a.log"),
            vec!["awk", "{print $2}", "a.log"]
        );
        assert_eq!(lex_tokens("echo a\\ b"), vec!["echo", "a b"]);
    }
}
