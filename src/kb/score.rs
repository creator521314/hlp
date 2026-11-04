//! 意图与 kb 示例的相关性评分（v0.4 设计 §2.2）。关键词匹配，不上向量检索。
//!
//! 评分是确定性纯函数；阈值判定在 `kb/mod.rs`。

use std::collections::HashSet;

use super::parse::{Example, Page};

/// 分词：ASCII 单词小写化按非字母数字切分；CJK 连续段取单字 + 相邻二字组。
pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    let flush_ascii = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.is_empty() {
            out.push(std::mem::take(buf).to_ascii_lowercase());
        }
    };
    let flush_cjk = |buf: &mut Vec<char>, out: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        for c in buf.iter() {
            out.push(c.to_string());
        }
        for w in buf.windows(2) {
            out.push(format!("{}{}", w[0], w[1]));
        }
        buf.clear();
    };

    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(c);
        } else if is_cjk(c) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(c);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}'
    )
}

/// 召回式评分：`recall = |意图token ∩ 描述token| / |意图token|`（空意图 → 0.0）。
/// 意图原文含页命令名（如 "find"）额外 +0.2；上限 1.0。
/// 单条评分（设计 §2.2 的公开接口）。批量查询走 `score_parts` 复用意图集合，
/// 本函数保留为参考实现与测试基准，故允许当前二进制中不直接调用。
#[allow(dead_code)]
pub fn score(intent_tokens: &[String], example: &Example, page: &Page) -> f64 {
    let intent_set: HashSet<&str> = intent_tokens.iter().map(String::as_str).collect();
    score_parts(
        &intent_set,
        &example.description,
        &example.command,
        &page.command,
    )
}

/// 批量打分入口：意图集合在 `Kb::lookup` 中构建一次后复用，入参直接借用
/// 数据库行里的字符串，避免每条示例重建 HashSet 与 clone 文本。
///
/// 与 `score` 完全等价——`score` 只是把 `Example`/`Page` 拆成三段后转调本函数。
pub fn score_parts(
    intent_set: &HashSet<&str>,
    example_desc: &str,
    example_cmd: &str,
    page_cmd: &str,
) -> f64 {
    if intent_set.is_empty() {
        return 0.0;
    }
    let mut target = tokenize(example_desc);
    target.extend(tokenize(example_cmd));
    target.extend(tokenize(page_cmd));
    let set: HashSet<&str> = target.iter().map(String::as_str).collect();
    let hit = intent_set.iter().filter(|t| set.contains(**t)).count();
    let recall = hit as f64 / intent_set.len() as f64;
    let bonus = if intent_set.contains(page_cmd) { 0.2 } else { 0.0 };
    (recall + bonus).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-56：CJK bigram 与 ASCII 词
    #[test]
    fn tu56_tokenize_cjk_bigrams_and_ascii() {
        let t = tokenize("列出大文件 find");
        for bigram in ["列出", "出大", "大文", "文件"] {
            assert!(t.contains(&bigram.to_string()), "应含 bigram {bigram}: {t:?}");
        }
        assert!(t.contains(&"find".to_string()), "{t:?}");
        assert!(t.contains(&"列".to_string()), "单字也应保留: {t:?}");
    }

    #[test]
    fn tokenize_ascii_is_lowercased_and_split() {
        assert_eq!(tokenize("Find -Name *.EXT"), vec!["find", "name", "ext"]);
    }

    fn fixture() -> Page {
        Page {
            command: "find".to_string(),
            description: "查找文件".to_string(),
            examples: vec![Example {
                description: "列出大文件".to_string(),
                command: "find . -size +100M".to_string(),
            }],
        }
    }

    /// T-U-57：全命中 / 半命中 / 命令名加成 / 封顶
    #[test]
    fn tu57_score_recall_and_bonus() {
        let page = fixture();
        let ex = &page.examples[0];
        // 意图 token 全被描述覆盖
        let full = tokenize("大文件");
        assert!(score(&full, ex, &page) >= 0.99, "全命中应为 1.0");
        // 半命中：一个命中一个未命中
        let half = tokenize("大文件 压缩");
        let s = score(&half, ex, &page);
        assert!(s > 0.0 && s < 1.0, "半命中应介于 0..1：{s}");
        // 意图含命令名 → +0.2，且封顶 1.0
        let with_name = tokenize("find 大文件");
        let s2 = score(&with_name, ex, &page);
        assert_eq!(s2, 1.0, "加成后封顶：{s2}");
        // 空意图 → 0.0
        assert_eq!(score(&[], ex, &page), 0.0);
    }

    /// 批量打分优化不得改变评分结果：score_parts 与 score 必须逐位一致。
    #[test]
    fn score_parts_is_equivalent_to_score() {
        let page = fixture();
        let ex = &page.examples[0];
        for intent in ["大文件", "find 大文件", "压缩 归档", "完全无关", ""] {
            let tokens = tokenize(intent);
            let set: HashSet<&str> = tokens.iter().map(String::as_str).collect();
            assert_eq!(
                score_parts(&set, &ex.description, &ex.command, &page.command),
                score(&tokens, ex, &page),
                "意图 {intent:?} 的批量打分应与单条打分一致"
            );
        }
    }
}
