use std::time::Duration;

use serde_json::{json, Value};

use crate::config::OllamaConfig;
use crate::error::HlpError;

use super::{Candidate, GenerateRequest, GenerateResponse, Provider};

/// `Provider` 的默认实现：调用本地 ollama 的 `POST /api/chat`（非流式）。
/// HTTP 契约与两级解析策略见 docs/design/v0.1-engine.md §2.4。
pub struct OllamaProvider {
    cfg: OllamaConfig,
    agent: ureq::Agent,
}

impl OllamaProvider {
    pub fn new(cfg: &OllamaConfig) -> Self {
        // 整体请求超时 = timeout_ms（默认 60s，覆盖 ollama 冷加载模型；
        // 遗留风险 #4：实现者不要把这个值改小）。
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build();
        Self {
            cfg: cfg.clone(),
            agent,
        }
    }
}

/// 透传给 ollama `format` 字段的 JSON Schema（设计文档 §3.2）。
/// maxItems 固定 10；实际候选上限由提示词与 generate 截断共同保证。
fn format_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "candidates": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": 10
            }
        },
        "required": ["candidates"]
    })
}

impl Provider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn generate(&self, req: &GenerateRequest) -> Result<GenerateResponse, HlpError> {
        let base_url = self.cfg.base_url.trim_end_matches('/').to_string();
        let url = format!("{base_url}/api/chat");
        let body = json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": req.system_prompt },
                { "role": "user", "content": req.intent },
            ],
            "stream": false,
            "format": format_schema(),
            "keep_alive": self.cfg.keep_alive,
            "options": { "temperature": 0.2 },
        });

        let resp = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| map_ureq_err(e, &base_url))?;

        let v: Value = resp
            .into_json()
            .map_err(|e| HlpError::NoCandidates {
                raw_excerpt: format!("响应体不是合法 JSON：{e}"),
            })?;
        let content = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| HlpError::NoCandidates {
                raw_excerpt: "响应缺少 message.content 字段".to_string(),
            })?;

        let commands: Vec<String> = extract_candidates(content)
            .into_iter()
            .take(req.max_candidates)
            .collect();
        if commands.is_empty() {
            return Err(HlpError::NoCandidates {
                raw_excerpt: excerpt_of(content),
            });
        }
        Ok(GenerateResponse {
            candidates: commands.into_iter().map(|command| Candidate { command }).collect(),
        })
    }
}

fn map_ureq_err(e: ureq::Error, base_url: &str) -> HlpError {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            HlpError::ProviderHttp {
                status: code,
                body_excerpt: excerpt_of(&body),
            }
        }
        ureq::Error::Transport(t) => HlpError::ProviderUnreachable {
            base_url: base_url.to_string(),
            reason: t.to_string(),
        },
    }
}

/// 截断到 200 字符，用于错误信息中的原始响应片段。
fn excerpt_of(s: &str) -> String {
    s.chars().take(200).collect()
}

/// 两级解析（设计文档 §4.2）：
/// 1. 主路径：content 为合法 JSON `{"candidates": [...]}`
/// 2. 兜底路径：JSON 解析失败或形状不对 → 按行切分提取命令
///
/// 注意：JSON 合法且 `candidates` 为数组（即使为空数组，如模型对恶意
/// 意图的安全拒绝）→ 直接返回主路径结果，不再兜底。
fn extract_candidates(content: &str) -> Vec<String> {
    if let Ok(v) = serde_json::from_str::<Value>(content.trim()) {
        if let Some(arr) = v.get("candidates").and_then(|c| c.as_array()) {
            return arr
                .iter()
                .filter_map(|x| x.as_str())
                .filter_map(clean_command)
                .collect();
        }
        // JSON 合法但缺 candidates 字段 / 不是数组 → 模型违约，落入兜底
    }
    fallback_lines(content)
}

/// 兜底解析：按行切分，剥壳后过滤非命令行。
fn fallback_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(clean_command)
        .filter(|line| looks_like_command(line))
        .collect()
}

/// 清洗单条候选：去围栏 / 剥行首序号与列表标记 / 剥反引号包裹 /
/// 折叠内部换行与空白为单空格。返回 None 表示清洗后为空。
fn clean_command(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    // 剥离 Markdown 围栏（``` 或 ```bash 开头 / ``` 结尾）
    while let Some(rest) = s.strip_prefix("```") {
        s = rest
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '-')
            .trim();
    }
    if let Some(rest) = s.strip_suffix("```") {
        s = rest.trim();
    }
    // 剥反引号包裹（仅当首尾成对；不碰命令内部的命令替换）
    if s.len() > 1 && s.starts_with('`') && s.ends_with('`') {
        s = &s[1..s.len() - 1];
        s = s.trim();
    }
    // 剥行首序号 / 列表标记（标记后必须跟空白，避免误伤 -al 这类参数）
    s = strip_list_marker(s);
    // 折叠内部换行/连续空白
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

fn strip_list_marker(s: &str) -> &str {
    let s = s.trim_start();
    if let Some(pos) = s.find(char::is_whitespace) {
        let head = s[..pos].trim_end();
        let is_numbered = head.len() >= 2
            && head.ends_with('.')
            && head[..head.len() - 1].chars().all(|c| c.is_ascii_digit());
        let is_bulleted = head == "-" || head == "*" || head == "+";
        if is_numbered || is_bulleted {
            return s[pos..].trim_start();
        }
    }
    s
}

/// "看起来像命令"的判定（兜底解析的过滤器）：
/// - 以中文句读结尾 → 说明性文字
/// - 解释性前缀开头 → 说明性文字
/// - JSON 对象片段（`{"` 开头）→ 模型违约输出的结构化残留
/// - 不含任何 ASCII 字符 → 大概率是纯中文说明
fn looks_like_command(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.ends_with('。') || s.ends_with('，') || s.ends_with('；')
        || s.ends_with('？') || s.ends_with('！')
    {
        return false;
    }
    const PROSE_PREFIXES: [&str; 8] = [
        "这条命令", "该命令", "这会", "其中", "说明", "解释", "注意", "命令解释",
    ];
    if PROSE_PREFIXES.iter().any(|p| s.starts_with(p)) {
        return false;
    }
    if s.starts_with("{\"") {
        return false;
    }
    if !s.chars().any(|c| c.is_ascii()) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-06：合法 JSON 解析，顺序保持
    #[test]
    fn tu06_parse_json_candidates() {
        let content = r#"{"candidates": ["find . -type f", "du -sh .", "ls -lS"]}"#;
        let out = extract_candidates(content);
        assert_eq!(
            out,
            vec![
                "find . -type f".to_string(),
                "du -sh .".to_string(),
                "ls -lS".to_string()
            ]
        );
    }

    /// T-U-08：清洗——围栏、空白、内部换行
    #[test]
    fn tu08_clean_fences_whitespace_newlines() {
        assert_eq!(
            clean_command("  ```bash\nfind . -type f\n```  ").unwrap(),
            "find . -type f"
        );
        // 内部换行折叠为单空格
        assert_eq!(
            clean_command("find . -type f \\\n  -size +100M").unwrap(),
            "find . -type f \\ -size +100M"
        );
        // 首尾空白
        assert_eq!(clean_command("  ls -l  ").unwrap(), "ls -l");
        // 空串
        assert!(clean_command("   ").is_none());
        assert!(clean_command("```\n```").is_none());
    }

    /// T-U-09：兜底解析过滤解释行，只保留命令行
    #[test]
    fn tu09_fallback_filters_prose() {
        let content = "1. find . -type f -size +100M\n2. du -sh .\n这条命令会查找所有大文件。\n用于列出文件大小。";
        let out = fallback_lines(content);
        assert_eq!(
            out,
            vec![
                "find . -type f -size +100M".to_string(),
                "du -sh .".to_string()
            ]
        );
    }

    /// 兜底触发：content 不是 JSON
    #[test]
    fn fallback_when_not_json() {
        let content = "```bash\nfind . -type f\n```\n- du -sh .";
        let out = extract_candidates(content);
        assert_eq!(out, vec!["find . -type f".to_string(), "du -sh .".to_string()]);
    }

    /// JSON 合法且 candidates 为空数组（如模型对恶意意图的安全拒绝）
    /// → 直接返回空，不再兜底
    #[test]
    fn empty_json_array_returns_empty() {
        let content = r#"{"candidates": []}"#;
        let out = extract_candidates(content);
        assert!(out.is_empty());
    }

    /// 混合输出（散文 + JSON）：JSON 解析整体失败时兜底，
    /// JSON 片段行不应混入候选
    #[test]
    fn mixed_prose_and_json_fallback() {
        let content = "好的，这是结果：\n{\"candidates\": [\"find . -type f\"]}";
        let out = extract_candidates(content);
        // JSON 整行被识别为结构化残留而过滤；散文行同样被过滤
        assert!(out.is_empty());
    }

    /// 遗留风险 #3 回归（单元级）：含危险参数的正常翻译原样通过，不被清洗逻辑篡改
    #[test]
    fn dangerous_command_not_mangled() {
        assert_eq!(
            clean_command("rm -rf /tmp/testdata").unwrap(),
            "rm -rf /tmp/testdata"
        );
        assert_eq!(
            clean_command("1. rm -rf /tmp/testdata").unwrap(),
            "rm -rf /tmp/testdata"
        );
    }
}
