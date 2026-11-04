//! 解释模式编排（v0.4 设计 §2.7 / §4.3）：拆段 → 本地标注 → LLM 一次性综合。
//!
//! 段对齐铁律（遗留风险 #6）：以本地 split 结果为骨架，LLM 段按序对位填充，
//! 缺段用 kb 标注、多段丢弃——绝不按 LLM 原始段序渲染。

pub mod lang;
pub mod split;

use serde::{Deserialize, Serialize};

pub use lang::Lang;

use crate::config::Config;
use crate::error::HlpError;
use crate::kb::{Kb, Page};
use crate::prompt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub cmd: String,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainSource {
    Local,
    Llm,
    Mixed,
}

impl ExplainSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainSource::Local => "local",
            ExplainSource::Llm => "llm",
            ExplainSource::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub segments: Vec<Segment>,
    pub lang: Lang,
    pub source: ExplainSource,
}

/// `-x` 主入口：offline=true → 只用 kb 本地标注（不碰 LLM）。
pub fn explain(cfg: &Config, command: &str, offline: bool) -> Result<Explanation, HlpError> {
    let lang = lang::detect(
        &cfg.explain.language,
        command,
        std::env::var("LC_ALL")
            .ok()
            .or_else(|| std::env::var("LANG").ok())
            .as_deref(),
    );
    let (local, materials) = local_segments(cfg, command);

    if offline {
        return Ok(Explanation {
            segments: local,
            lang,
            source: ExplainSource::Local,
        });
    }

    // 决策 6 + 12：默认经 daemon v2 Explain；版本错/断连 → 直连；再败 → 本地标注兜底
    let llm = match crate::daemon::client::explain(cfg, command, lang, offline) {
        Ok(Some(exp)) => Some(exp.segments),
        // 任何 daemon 失败（含 v1 协议错）→ 回退直连 Provider（决策 12）
        _ => crate::explain::llm_explain(cfg, command, &local, &materials, lang).ok(),
    };
    match llm {
        Some(remote) => {
            let merged = merge_segments(&local, &remote);
            // 全部段都由 LLM 注解 → Llm；有段回落到 kb 标注 → Mixed
            let source = if remote.len() >= local.len()
                && remote.iter().take(local.len()).all(|s| !s.note.trim().is_empty())
            {
                ExplainSource::Llm
            } else {
                ExplainSource::Mixed
            };
            Ok(Explanation {
                segments: merged,
                lang,
                source,
            })
        }
        None => {
            let has_annotation = local.iter().any(|s| !s.note.is_empty());
            if has_annotation {
                Ok(Explanation {
                    segments: local,
                    lang,
                    source: ExplainSource::Local,
                })
            } else {
                Err(HlpError::ProviderUnreachable {
                    base_url: cfg.ollama.base_url.clone(),
                    reason: "解释模式需要 LLM（可加 --offline 只用本地知识库标注）".to_string(),
                })
            }
        }
    }
}

/// 本地标注骨架：每段取 kb 页描述作为 note（无页 → 空串）。
pub fn explain_direct(cfg: &Config, command: &str, offline: bool) -> Result<Explanation, HlpError> {
    let lang = lang::detect(
        &cfg.explain.language,
        command,
        std::env::var("LC_ALL")
            .ok()
            .or_else(|| std::env::var("LANG").ok())
            .as_deref(),
    );
    let (local, materials) = local_segments(cfg, command);
    if offline {
        return Ok(Explanation {
            segments: local,
            lang,
            source: ExplainSource::Local,
        });
    }
    match llm_explain(cfg, command, &local, &materials, lang) {
        Ok(remote) => {
            let merged = merge_segments(&local, &remote);
            let all_llm = remote.len() >= local.len();
            Ok(Explanation {
                segments: merged,
                lang,
                source: if all_llm {
                    ExplainSource::Llm
                } else {
                    ExplainSource::Mixed
                },
            })
        }
        Err(e) => {
            if local.iter().any(|s| !s.note.is_empty()) {
                Ok(Explanation {
                    segments: local,
                    lang,
                    source: ExplainSource::Local,
                })
            } else {
                Err(e)
            }
        }
    }
}

/// 本地标注骨架：每段取 kb 页描述作为 note（无页 → 空串）。
/// 返回值之二为提示词素材（段号、命令、页内容截断）。
fn local_segments(cfg: &Config, command: &str) -> (Vec<Segment>, Vec<String>) {
    let kb = Kb::from_config(cfg).ok();
    let mut segments = Vec::new();
    let mut materials = Vec::new();
    for seg in split::split_pipeline(command) {
        let note = match (&kb, split::base_command(&seg)) {
            (Some(k), Some(base)) => k
                .page_of(&base)
                .map(|p| p.description)
                .unwrap_or_default(),
            _ => String::new(),
        };
        let material = kb
            .as_ref()
            .and_then(|k| split::base_command(&seg).and_then(|b| k.page_of(&b)))
            .map(|p| page_material(&p))
            .unwrap_or_else(|| "（无本地知识库页）".to_string());
        materials.push(format!("[{}] {seg}\n{material}", segments.len() + 1));
        segments.push(Segment { cmd: seg, note });
    }
    (segments, materials)
}

fn page_material(page: &Page) -> String {
    let mut s = format!("知识库页 {}：{}\n", page.command, page.description);
    for ex in page.examples.iter().take(5) {
        s.push_str(&format!("  - {}：`{}`\n", ex.description, ex.command));
    }
    s.chars().take(600).collect()
}

/// 段对齐合成（T-U-67 守门）：骨架 = 本地 split；LLM 段按序取 note，
/// 缺段保留 kb 标注，多出的丢弃；段序与 cmd 永不被 LLM 改写。
pub fn merge_segments(local: &[Segment], remote: &[Segment]) -> Vec<Segment> {
    local
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let note = remote
                .get(i)
                .map(|r| r.note.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| seg.note.clone());
            Segment {
                cmd: seg.cmd.clone(),
                note,
            }
        })
        .collect()
}

/// 一次 LLM 调用（全部段 + kb 素材一次喂入，决策 6）。
/// 解析复用 v0.1 两级哲学：JSON 失败 → 按行兜底对齐段序。
pub fn llm_explain(
    cfg: &Config,
    command: &str,
    local: &[Segment],
    materials: &[String],
    lang: Lang,
) -> Result<Vec<Segment>, HlpError> {
    let system = prompt::build_explain_system_prompt(local, materials, lang);
    let raw = crate::provider::ollama::raw_chat(cfg, &system, command)?;
    Ok(parse_segments(&raw, local.len()))
}

/// 解析 LLM 输出：主路径 segments JSON；失败按行兜底（每行一条 note）。
fn parse_segments(raw: &str, n: usize) -> Vec<Segment> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(arr) = v.get("segments").and_then(|s| s.as_array()) {
            return arr
                .iter()
                .take(n)
                .map(|x| Segment {
                    cmd: x.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    note: x
                        .get("note")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect();
        }
    }
    // 行兜底：跳过空行/围栏/看起来像 JSON 的行
    trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('`') && !l.starts_with('{'))
        .take(n)
        .map(|l| Segment {
            cmd: String::new(),
            note: l.to_string(),
        })
        .collect()
}

/// §3.3 stdout 渲染：首行原样回显命令 → 空行 → 每段两空格缩进对齐。
pub fn render(command: &str, exp: &Explanation) -> String {
    let mut out = format!("{command}\n\n");
    let width = exp
        .segments
        .iter()
        .map(|s| s.cmd.chars().count())
        .max()
        .unwrap_or(0);
    for seg in &exp.segments {
        let note = if seg.note.is_empty() {
            "（无本地注解）"
        } else {
            seg.note.as_str()
        };
        let pad = width.saturating_sub(seg.cmd.chars().count());
        out.push_str(&format!(
            "  {}{}  {}\n",
            seg.cmd,
            " ".repeat(pad),
            note
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-67：LLM 缺一段 → 用 kb 标注兜底，段序不变
    #[test]
    fn tu67_missing_llm_segment_falls_back_to_local() {
        let local = vec![
            Segment { cmd: "find .".to_string(), note: "查找文件".to_string() },
            Segment { cmd: "sort".to_string(), note: "排序".to_string() },
            Segment { cmd: "uniq -c".to_string(), note: String::new() },
        ];
        let remote = vec![
            Segment { cmd: "wrong".to_string(), note: "递归查找".to_string() },
            Segment { cmd: "x".to_string(), note: "按行排序".to_string() },
        ];
        let merged = merge_segments(&local, &remote);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].cmd, "find .");
        assert_eq!(merged[0].note, "递归查找"); // LLM 覆盖
        assert_eq!(merged[1].note, "按行排序");
        assert_eq!(merged[2].cmd, "uniq -c");
        assert_eq!(merged[2].note, ""); // 缺段保留本地标注
        // 多出的 LLM 段被丢弃
        let extra = vec![Segment { cmd: "a".into(), note: "n".into() }; 8];
        assert_eq!(merge_segments(&local, &extra).len(), 3);
    }

    #[test]
    fn parse_segments_json_and_line_fallback() {
        let raw = r#"{"segments":[{"cmd":"a","note":"甲"},{"cmd":"b","note":"乙"}]}"#;
        let segs = parse_segments(raw, 3);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].note, "甲");
        let lines = "第一段说明\n```\n第二段说明\n";
        let segs = parse_segments(lines, 3);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].note, "第二段说明");
    }

    #[test]
    fn render_has_command_blank_line_and_segments() {
        let exp = Explanation {
            segments: vec![
                Segment { cmd: "find .".to_string(), note: "查找".to_string() },
                Segment { cmd: "sort -rh".to_string(), note: String::new() },
            ],
            lang: Lang::Zh,
            source: ExplainSource::Mixed,
        };
        let text = render("find . | sort -rh", &exp);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "find . | sort -rh");
        assert_eq!(lines[1], "");
        assert!(lines[2].starts_with("  find ."));
        assert!(lines[2].contains("查找"));
        assert!(lines[3].contains("（无本地注解）"));
    }
}
