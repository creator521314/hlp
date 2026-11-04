//! daemon 服务端主循环（v0.3 设计 §2.2）。
//!
//! - 串行 accept（决策 4）：人按 Ctrl+G 的使用模式本质串行
//! - 空闲 `idle_secs`（默认 300s）无请求 → 清理 socket 文件并退出码 0
//! - 让位（遗留风险 #5）：bind 失败且已有存活实例 → 退出码 0 静默让位，
//!   **不是错误**——否则 ensure_running 会误判拉起失败，daemon 永远起不来

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::Config;
use crate::daemon::client;
use crate::daemon::protocol::{
    ErrorKind, Event, Op, Request, Source, PROTOCOL_VERSION,
};
use crate::daemon::socket_path;
use crate::error::HlpError;
use crate::prompt;
use crate::provider::{self, Provider, GenerateRequest};

/// 单行请求的读取上限：防异常客户端无限占用连接。
const MAX_REQUEST_BYTES: usize = 1 << 20;

/// 退出方式（两者对调用方都是"成功"）。
#[derive(Debug, PartialEq, Eq)]
pub enum ServeResult {
    /// 空闲超时正常退出（socket 已清理）
    Idle,
    /// 发现已存活实例，让位（bind 未发生，无需清理）
    Yielded,
}

/// daemon 主循环（`hlp daemon` 子命令调用，阻塞至退出）。
pub fn serve(cfg: &Config) -> Result<ServeResult, HlpError> {
    let path = socket_path(cfg);
    let listener = match bind_or_yield(cfg, &path)? {
        Some(l) => l,
        None => return Ok(ServeResult::Yielded),
    };
    // 数据只归本用户（§3.4：socket 0600）
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));

    let provider = provider::from_config(cfg)?;
    let mut cache = Cache::open_best_effort(cfg); // 缓存故障不影响服务（决策 3）

    let idle = Duration::from_secs(cfg.daemon.idle_secs);
    listener
        .set_nonblocking(true)
        .map_err(|e| HlpError::DaemonStart { reason: format!("nonblocking 设置失败：{e}") })?;

    let mut last_active = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                handle_connection(stream, cfg, &mut cache, provider.as_ref());
                last_active = Instant::now();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last_active.elapsed() >= idle {
                    break; // 空闲超时退出
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    // 清理 socket 文件再退出
    let _ = fs::remove_file(&path);
    Ok(ServeResult::Idle)
}

/// bind 冲突的两种处置：对端存活 → None（让位）；死 socket → 删文件重 bind。
fn bind_or_yield(cfg: &Config, path: &Path) -> Result<Option<UnixListener>, HlpError> {
    match UnixListener::bind(path) {
        Ok(l) => Ok(Some(l)),
        Err(_) => {
            if client::ping(cfg) {
                return Ok(None); // 让位：已有存活实例
            }
            let _ = fs::remove_file(path); // 残留死 socket
            UnixListener::bind(path)
                .map(Some)
                .map_err(|e| HlpError::DaemonStart {
                    reason: format!("socket bind 失败 {}: {e}", path.display()),
                })
        }
    }
}

/// 处理一条连接：读一行 Request → 回零或多行 Event → 终结。
/// 单连接处理失败不退出主循环（§2.2 失败语义）。
fn handle_connection(
    stream: UnixStream,
    cfg: &Config,
    cache: &mut Cache,
    provider: &dyn Provider,
) {
    // 读请求前设 5s 读超时：异常客户端不得永久占用串行 accept
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);

    let line = match read_limited(&mut reader, MAX_REQUEST_BYTES) {
        Ok(l) if !l.is_empty() => l,
        _ => return, // EOF / 超时 / 超长 → 直接断连
    };

    let req: Request = match serde_json::from_str(line.trim_end()) {
        Ok(r) => r,
        Err(e) => {
            let _ = send_event(&mut writer, &Event::Error {
                kind: ErrorKind::Protocol,
                message: format!("请求解析失败：{e}"),
            });
            return;
        }
    };
    if req.v != PROTOCOL_VERSION {
        let _ = send_event(&mut writer, &Event::Error {
            kind: ErrorKind::Protocol,
            message: format!(
                "协议版本不匹配：daemon={PROTOCOL_VERSION}，客户端={}",
                req.v
            ),
        });
        return;
    }

    match req.op {
        Op::Ping => {
            let _ = send_event(&mut writer, &Event::Ok);
        }
        Op::Explain {
            command,
            lang,
            offline,
        } => {
            // 服务端复用与直连相同的合成逻辑（本地标注 + 一次 LLM），不经 socket 递归
            match crate::explain::explain_direct(cfg, &command, offline) {
                Ok(mut exp) => {
                    exp.lang = lang;
                    let _ = send_event(
                        &mut writer,
                        &Event::Explanation {
                            segments: exp.segments,
                            source: exp.source,
                        },
                    );
                }
                Err(e) => {
                    let _ = send_event(&mut writer, &Event::Error {
                        kind: ErrorKind::Protocol,
                        message: e.to_string(),
                    });
                }
            }
        }
        Op::Clear => {
            match cache.clear() {
                Ok(()) => { let _ = send_event(&mut writer, &Event::Ok); }
                Err(e) => { let _ = send_event(&mut writer, &cache_error(&e)); }
            }
        }
        Op::Learn {
            intent,
            command,
            cwd,
            source,
        } => {
            match cache.learn_strict(&intent, &command, source, &cwd) {
                Ok(()) => { let _ = send_event(&mut writer, &Event::Ok); }
                Err(e) => { let _ = send_event(&mut writer, &cache_error(&e)); }
            }
        }
        Op::Generate {
            intent,
            max_candidates,
            cwd: _cwd, // 豁免只约束 learn；lookup 不看 cwd
            stream,
        } => {
            // 先查缓存：命中 → done(cache)，不碰 LLM（§2.2）
            if let Some((entry, _)) = cache.lookup(&intent) {
                let _ = send_event(
                    &mut writer,
                    &Event::Done {
                        candidates: vec![entry.command],
                        source: Source::Cache,
                    },
                );
                return;
            }
            // 未命中 → Provider。成功后**不落缓存**（决策 6：学习只经 cache-learn）
            let max = max_candidates.clamp(1, 10);
            let gen_req = GenerateRequest {
                intent,
                max_candidates: max,
                system_prompt: prompt::build_system_prompt(max),
                want_explanation: false,
            };
            let result = if stream {
                // 逐快照转发；客户端断连（写失败）→ 停止转发但不报错
                let mut sink = |text: &str| {
                    let _ = send_event(&mut writer, &Event::Chunk { text: text.to_string() });
                };
                provider.generate_stream(&gen_req, &mut sink)
            } else {
                provider.generate(&gen_req)
            };
            match result {
                Ok(resp) => {
                    let candidates: Vec<String> =
                        resp.candidates.iter().map(|c| c.command.clone()).collect();
                    let _ = send_event(
                        &mut writer,
                        &Event::Done {
                            candidates,
                            source: Source::Llm,
                        },
                    );
                }
                Err(e) => {
                    let _ = send_event(&mut writer, &Event::Error {
                        kind: match &e {
                            HlpError::ProviderUnreachable { .. } | HlpError::ProviderHttp { .. } => {
                                ErrorKind::ProviderUnreachable
                            }
                            HlpError::NoCandidates { .. } => ErrorKind::NoCandidates,
                            _ => ErrorKind::Protocol,
                        },
                        message: e.to_string(),
                    });
                }
            }
        }
    }
}

/// 缓存操作失败 → 借道 protocol 类错误（客户端换译为 CacheUnavailable / 退出码 6）。
fn cache_error(e: &HlpError) -> Event {
    Event::Error {
        kind: ErrorKind::Protocol,
        message: format!("cache: {e}"),
    }
}

/// 读一行（按字节累积到 `\n`，再整体转 UTF-8——多字节中文不能逐 byte push），
/// 累计字节数超上限时返回 Err。
fn read_limited(
    reader: &mut BufReader<UnixStream>,
    max: usize,
) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 => break,
            _ => {
                if bytes.len() >= max {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "请求行超长",
                    ));
                }
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
        }
    }
    String::from_utf8(bytes).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })
}

/// 序列化 + 单行写出 + flush（NDJSON 帧边界由 flush 保证）。
fn send_event(w: &mut UnixStream, ev: &Event) -> std::io::Result<()> {
    let mut line = serde_json::to_string(ev)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}
