//! socket 协议类型（v0.3 设计 §2.1）：NDJSON——每行一个 JSON 对象。
//!
//! 契约：连接建立后客户端**先发一行 Request**，daemon 回零或多行 Event，
//! 以 `Done` / `Ok` / `Error` 之一终结并关闭连接。`Chunk.text` 是**全量快照**
//! （非增量 delta），消费方直接整体替换 buffer。
//!
//! 版本协商：`v` 不匹配 → daemon 回 `Error{protocol}`，客户端回退直连（决策 5）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;

/// 客户端 → daemon，单行 JSON。`op` 以内部标签平铺到顶层（§3.2 帧样例）。
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// 协议版本；不匹配 → daemon 回 protocol 错误
    pub v: u32,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Generate {
        intent: String,
        max_candidates: usize,
        cwd: PathBuf,
        stream: bool,
    },
    /// 学习回流（preexec 钩子）；豁免目录由 daemon 侧判
    Learn {
        intent: String,
        command: String,
        cwd: PathBuf,
        source: crate::cache::store::Source,
    },
    Clear,
    Ping,
    /// v0.4：解释模式（daemon 侧一次性 LLM + kb 本地标注兜底）
    Explain {
        command: String,
        lang: crate::explain::Lang,
        offline: bool,
    },
}

/// daemon → 客户端，每行一个。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// 当前最优候选快照（全量替换语义，只出现在 stream 请求中）
    Chunk { text: String },
    /// 终结：候选集 + 来源（cache | llm）
    Done {
        candidates: Vec<String>,
        source: Source,
    },
    /// Learn / Clear / Ping 的应答
    Ok,
    /// v0.4：Explain 的终结事件
    Explanation {
        segments: Vec<crate::explain::Segment>,
        source: crate::explain::ExplainSource,
    },
    Error { kind: ErrorKind, message: String },
}

/// 生成来源（verbose 元信息也用它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Cache,
    Kb,
    Llm,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Cache => "cache",
            Source::Kb => "kb",
            Source::Llm => "llm",
        }
    }
}

/// daemon 上报的错误类别。`protocol` 覆盖版本不匹配与缓存操作失败
/// （Learn/Clear 的缓存错误也借道 protocol，客户端换译为 CacheUnavailable）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    ProviderUnreachable,
    NoCandidates,
    Protocol,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 帧样例与设计 §3.2 一致（含扁平 op 与小写标签）
    #[test]
    fn request_generate_wire_format() {
        let req = Request {
            v: PROTOCOL_VERSION,
            op: Op::Generate {
                intent: "列出大文件".to_string(),
                max_candidates: 3,
                cwd: PathBuf::from("/home/x"),
                stream: true,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"v":2,"op":"generate","intent":"列出大文件","max_candidates":3,"cwd":"/home/x","stream":true}"#
        );
        // 反向解析
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request { v: 2, op: Op::Generate { stream: true, .. } }));
    }

    #[test]
    fn unit_ops_wire_format() {
        let ping = serde_json::to_string(&Request { v: PROTOCOL_VERSION, op: Op::Ping }).unwrap();
        assert_eq!(ping, r#"{"v":2,"op":"ping"}"#);
        let back: Request = serde_json::from_str(&ping).unwrap();
        assert!(matches!(back.op, Op::Ping));

        let clear = serde_json::to_string(&Request { v: PROTOCOL_VERSION, op: Op::Clear }).unwrap();
        assert_eq!(clear, r#"{"v":2,"op":"clear"}"#);

        let learn = serde_json::to_string(&Request {
            v: PROTOCOL_VERSION,
            op: Op::Learn {
                intent: "i".to_string(),
                command: "c".to_string(),
                cwd: PathBuf::from("/tmp"),
                source: crate::cache::store::Source::Adopted,
            },
        })
        .unwrap();
        assert!(learn.contains(r#""source":"adopted""#));
        let back: Request = serde_json::from_str(&learn).unwrap();
        assert!(matches!(back.op, Op::Learn { source: crate::cache::store::Source::Adopted, .. }));
    }

    /// 事件帧样例（设计 §3.2）
    #[test]
    fn event_wire_format() {
        let chunk: Event = serde_json::from_str(r#"{"type":"chunk","text":"find . -ty"}"#).unwrap();
        assert!(matches!(chunk, Event::Chunk { ref text } if text == "find . -ty"));

        let done: Event = serde_json::from_str(
            r#"{"type":"done","candidates":["find . -type f -size +100M"],"source":"llm"}"#,
        )
        .unwrap();
        assert!(matches!(done, Event::Done { source: Source::Llm, .. }));

        let ok: Event = serde_json::from_str(r#"{"type":"ok"}"#).unwrap();
        assert!(matches!(ok, Event::Ok));

        let err: Event = serde_json::from_str(
            r#"{"type":"error","kind":"provider_unreachable","message":"x"}"#,
        )
        .unwrap();
        assert!(matches!(err, Event::Error { kind: ErrorKind::ProviderUnreachable, .. }));
    }

    /// T-U-66：协议 v2 Explain / Explanation 往返；v1 请求解析后将被 server 拒绝
    #[test]
    fn tu66_explain_wire_roundtrip() {
        use crate::explain::{lang::Lang, ExplainSource, Segment};
        let req = Request {
            v: PROTOCOL_VERSION,
            op: Op::Explain {
                command: "find . | sort".to_string(),
                lang: Lang::Zh,
                offline: true,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"v":2,"op":"explain","command":"find . | sort","lang":"zh","offline":true}"#
        );
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.op, Op::Explain { offline: true, .. }));

        let ev = Event::Explanation {
            segments: vec![Segment {
                cmd: "find .".to_string(),
                note: "查找".to_string(),
            }],
            source: ExplainSource::Mixed,
        };
        let line = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            line,
            r#"{"type":"explanation","segments":[{"cmd":"find .","note":"查找"}],"source":"mixed"}"#
        );
        let back: Event = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, Event::Explanation { .. }));

        // v1 请求仍可解析（版本检查在语义层，server 会回 protocol 错误）
        let old: Request = serde_json::from_str(r#"{"v":1,"op":"ping"}"#).unwrap();
        assert_eq!(old.v, 1);
        assert_ne!(old.v, PROTOCOL_VERSION);
    }

    /// 版本不匹配的请求仍可解析（版本检查在语义层做，不依赖解析失败）
    #[test]
    fn version_mismatch_parses_then_rejected() {
        let req: Request = serde_json::from_str(
            r#"{"v":99,"op":"ping"}"#,
        )
        .unwrap();
        assert_eq!(req.v, 99);
    }
}
