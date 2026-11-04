//! tldr-pages markdown 单页解析（v0.4 设计 §2.1）。
//!
//! 官方页结构（只接受此形态，容忍空行与行尾空白差异）：
//! ```markdown
//! # find
//!
//! > 递归查找目录树中的文件。
//! > More information: <https://...>.
//!
//! - 按扩展名查找文件：
//!
//! `find {{根路径}} -name '{{*.ext}}'`
//! ```

/// 一页 tldr 语料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// 标题（# 后内容），小写
    pub command: String,
    /// 第一条 `>` 引用（跳过 More information 行）
    pub description: String,
    pub examples: Vec<Example>,
}

/// 一条示例。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    /// `-` 列表项文字（去尾部冒号）
    pub description: String,
    /// 反引号内命令，`{{占位符}}` 原样保留
    pub command: String,
}

/// 解析单页 markdown；结构不合（无标题/无示例）→ Err(原因)。
pub fn parse_page(markdown: &str) -> Result<Page, String> {
    let mut command: Option<String> = None;
    let mut description = String::new();
    let mut examples: Vec<Example> = Vec::new();
    let mut pending_desc: Option<String> = None;

    for raw in markdown.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(title) = line.strip_prefix("# ") {
            if command.is_none() {
                command = Some(title.trim().to_ascii_lowercase());
            }
            continue;
        }
        if let Some(quote) = line.strip_prefix('>') {
            let q = quote.trim();
            if description.is_empty() && !q.starts_with("More information:") && !q.is_empty() {
                description = q.to_string();
            }
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            // 新示例开始；上一条若没等到命令则自然丢弃（不完整的列表项）
            pending_desc = Some(item.trim().trim_end_matches(['：', ':']).trim().to_string());
            continue;
        }
        // 反引号包裹的命令行（可能带语言标记，如 ```bash 不支持——tldr 用单反引号）
        if line.starts_with('`') && line.ends_with('`') && line.len() > 2 {
            let cmd = line[1..line.len() - 1].trim().to_string();
            if let Some(desc) = pending_desc.take() {
                examples.push(Example {
                    description: desc,
                    command: cmd,
                });
            }
        }
    }

    let command = command.ok_or_else(|| "缺少 # 标题".to_string())?;
    if examples.is_empty() {
        return Err("没有解析到任何示例".to_string());
    }
    Ok(Page {
        command,
        description,
        examples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-55：fixture 解析；More information 不入 description；占位符保留
    #[test]
    fn tu55_parse_page_fixture() {
        let md = r#"# find

> 递归查找目录树中的文件。
> More information: <https://github.com/tldr-pages/tldr>.

- 按扩展名查找文件：

`find {{根路径}} -name '{{*.ext}}'`

- 按大小查找：

`find {{根路径}} -size +100M`
"#;
        let page = parse_page(md).unwrap();
        assert_eq!(page.command, "find");
        assert_eq!(page.description, "递归查找目录树中的文件。");
        assert!(!page.description.contains("More information"));
        assert_eq!(page.examples.len(), 2);
        assert_eq!(page.examples[0].description, "按扩展名查找文件");
        assert_eq!(
            page.examples[0].command,
            "find {{根路径}} -name '{{*.ext}}'"
        );
        assert_eq!(page.examples[1].description, "按大小查找");
    }

    #[test]
    fn parse_rejects_broken_structure() {
        assert!(parse_page("没有标题\n> 描述\n").is_err());
        assert!(parse_page("# find\n> 只有描述\n").is_err());
        assert!(parse_page("# Find\n\n- 示例:\n\n`FIND {{x}}`\n").unwrap().command == "find");
    }

    #[test]
    fn parse_tolerates_blank_lines_and_trailing_spaces() {
        let md = "# ls   \n\n  > 列出文件。  \n\n - 列出全部:  \n\n `ls -la`  \n";
        let p = parse_page(md).unwrap();
        assert_eq!(p.command, "ls");
        assert_eq!(p.description, "列出文件。");
        assert_eq!(p.examples[0].command, "ls -la");
    }
}
