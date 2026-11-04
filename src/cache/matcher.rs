//! 意图归一化与匹配策略（v0.3 设计 §2.6，§14 开放问题的 v0.3 解答）。
//!
//! 两级匹配：先精确，后前缀。前缀要求已存意图长度 ≥ 4 字符且为当前意图的
//! 真前缀，取最长者。模糊匹配（编辑距离/向量）延后——需要评分阈值实验。

/// 归一化：trim + 折叠连续空白为单空格 + ASCII 小写化（中文不受影响）。
/// 这是缓存键：同一条意图无论怎么输入，归一化后必须相同。
pub fn normalize_intent(s: &str) -> String {
    let lowered: String = s.chars().map(|c| c.to_ascii_lowercase()).collect();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 命中方式（verbose 元信息用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Prefix,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Prefix => "prefix",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-41：trim + 空白折叠 + ASCII 小写化；中文不受影响
    #[test]
    fn tu41_normalize() {
        assert_eq!(normalize_intent("  列出  大文件 "), "列出 大文件");
        assert_eq!(normalize_intent("LS -L"), "ls -l");
        assert_eq!(normalize_intent("把  当前目录\t文件  列出来"), "把 当前目录 文件 列出来");
        assert_eq!(normalize_intent("  "), "");
    }
}
