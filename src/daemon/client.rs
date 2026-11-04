//! CLI 侧 socket 客户端（v0.3 设计 §2.4）。
//!
//! 失败语义分两档（决策 5）：
//! - `Transient`：基础设施问题（连不上 / 协议错 / 断连）——调用方**回退直连**；
//! - `Permanent`：daemon 已代表我们完成过生成并给出结论（provider 类错误）——
//!   回退只会重复同样的失败，直接上抛。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::cache::store::Source as LearnSource;
use crate::cache::db_path;
use crate::config::Config;
use crate::daemon::protocol::{
    ErrorKind, Event, Op, Request, Source, PROTOCOL_VERSION,
};
use crate::daemon::socket_path;
use crate::error::HlpError;
use crate::provider::{Candidate, GenerateRequest, GenerateResponse};

/// daemon 路径失败：是否值得回退直连。
pub enum Failure {
    /// 回退直连
    Transient(HlpError),
    /// 直接上抛
    Permanent(HlpError),
}

/// 健康检查：socket 可连且 Ping 通。任何失败一律 false（不区分原因）。
pub fn ping(cfg: &Config) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path(cfg)) else {
        return false;
    };
    if send_request(&mut stream, &Request { v: PROTOCOL_VERSION, op: Op::Ping }).is_err() {
        return false;
    }
    matches!(read_one_event(&mut stream), Some(Event::Ok))
}

/// 经 daemon 生成；`on_chunk` 逐快照触发（全量替换语义）。
pub fn generate(
    cfg: &Config,
    req: &GenerateRequest,
    cwd: &Path,
    stream: bool,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<(GenerateResponse, Source), Failure> {
    let mut sock = connect(cfg)?;
    send_request(
        &mut sock,
        &Request {
            v: PROTOCOL_VERSION,
            op: Op::Generate {
                intent: req.intent.clone(),
                max_candidates: req.max_candidates,
                cwd: cwd.to_path_buf(),
                stream,
            },
        },
    )?;

    loop {
        match read_one_event(&mut sock) {
            None => {
                return Err(Failure::Transient(HlpError::DaemonStart {
                    reason: "daemon 在终结事件前断连".to_string(),
                }))
            }
            Some(Event::Chunk { text }) => on_chunk(&text),
            Some(Event::Done { candidates, source }) => {
                let candidates: Vec<Candidate> =
                    candidates.into_iter().map(|command| Candidate { command }).collect();
                if candidates.is_empty() {
                    return Err(Failure::Permanent(HlpError::NoCandidates {
                        raw_excerpt: "daemon 返回空候选".to_string(),
                    }));
                }
                let response = GenerateResponse { candidates, segments: Vec::new() };
                return Ok((response, source));
            }
            Some(Event::Ok) => {
                return Err(Failure::Transient(HlpError::DaemonStart {
                    reason: "协议错位：期待 done，收到 ok".to_string(),
                }))
            }
            Some(Event::Explanation { .. }) => {
                return Err(Failure::Transient(HlpError::DaemonStart {
                    reason: "协议错位：期待 done，收到 explanation".to_string(),
                }))
            }
            Some(Event::Error { kind, message }) => {
                let err = match kind {
                    ErrorKind::ProviderUnreachable => HlpError::ProviderUnreachable {
                        base_url: cfg.ollama.base_url.clone(),
                        reason: format!("经 daemon：{message}"),
                    },
                    ErrorKind::NoCandidates => HlpError::NoCandidates { raw_excerpt: message },
                    ErrorKind::Protocol => HlpError::DaemonStart {
                        reason: format!("daemon 协议错误：{message}"),
                    },
                };
                return Err(match kind {
                    // 协议错 → 重试（直连）可能自愈
                    ErrorKind::Protocol => Failure::Transient(err),
                    _ => Failure::Permanent(err),
                });
            }
        }
    }
}

/// v0.4：经 daemon v2 Explain 取解释。
/// None = daemon 不可用/版本不匹配/执行失败 → 调用方回退直连 Provider（决策 12）。
pub fn explain(
    cfg: &Config,
    command: &str,
    lang: crate::explain::Lang,
    offline: bool,
) -> Result<Option<crate::explain::Explanation>, HlpError> {
    // ping 用当前协议版本：旧 v1 daemon 会回 protocol 错误 → false → 直连
    if !ping(cfg) {
        return Ok(None);
    }
    let mut sock = match connect(cfg) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let req = Request {
        v: PROTOCOL_VERSION,
        op: Op::Explain {
            command: command.to_string(),
            lang,
            offline,
        },
    };
    if send_request(&mut sock, &req).is_err() {
        return Ok(None);
    }
    Ok(match read_one_event(&mut sock) {
        Some(Event::Explanation { segments, source }) => Some(crate::explain::Explanation {
            segments,
            lang,
            source,
        }),
        // 任何其它应答（protocol / provider 错误、断连）→ 回退直连
        _ => None,
    })
}

/// 经 daemon 学习回流（daemon 不在时调用方直写 DB）。
pub fn learn(
    cfg: &Config,
    intent: &str,
    command: &str,
    source: LearnSource,
    cwd: &Path,
) -> Result<(), HlpError> {
    simple_op(
        cfg,
        Request {
            v: PROTOCOL_VERSION,
            op: Op::Learn {
                intent: intent.to_string(),
                command: command.to_string(),
                cwd: cwd.to_path_buf(),
                source,
            },
        },
    )
}

/// 经 daemon 清空缓存。
pub fn clear(cfg: &Config) -> Result<(), HlpError> {
    simple_op(cfg, Request { v: PROTOCOL_VERSION, op: Op::Clear })
}

/// Learn / Clear：期待一行 `Ok`；daemon 报缓存故障 → 换译 CacheUnavailable（退出码 6）。
fn simple_op(cfg: &Config, req: Request) -> Result<(), HlpError> {
    let mut sock = connect(cfg).map_err(unpack_transient)?;
    send_request(&mut sock, &req).map_err(|f| match f {
        Failure::Transient(e) => e,
        _ => unreachable!("send_request 只产生 Transient"),
    })?;
    match read_one_event(&mut sock) {
        Some(Event::Ok) => Ok(()),
        Some(Event::Error { message, .. }) => Err(HlpError::CacheUnavailable {
            path: db_path(),
            reason: format!("经 daemon：{message}"),
        }),
        _ => Err(HlpError::DaemonStart {
            reason: "daemon 应答异常".to_string(),
        }),
    }
}

fn unpack_transient(f: Failure) -> HlpError {
    match f {
        Failure::Transient(e) => e,
        Failure::Permanent(e) => e,
    }
}

fn connect(cfg: &Config) -> Result<UnixStream, Failure> {
    UnixStream::connect(socket_path(cfg)).map_err(|e| {
        Failure::Transient(HlpError::DaemonStart {
            reason: format!("socket 连接失败：{e}"),
        })
    })
}

fn send_request(sock: &mut UnixStream, req: &Request) -> Result<(), Failure> {
    let transient = |reason: String| {
        Failure::Transient(HlpError::DaemonStart { reason })
    };
    let mut line = serde_json::to_string(req)
        .map_err(|e| transient(format!("请求序列化失败：{e}")))?;
    line.push('\n');
    sock.write_all(line.as_bytes())
        .and_then(|_| sock.flush())
        .map_err(|e| transient(format!("请求写入失败：{e}")))
}

/// 读一行并解析为 Event；EOF / 读错 / 解析错 → None（调用方按断连处理）。
fn read_one_event(sock: &mut UnixStream) -> Option<Event> {
    let mut reader = BufReader::new(sock.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line.trim_end()).ok()
}
