//! 集成测试：mock ollama HTTP server + XDG_CONFIG_HOME 注入配置。
//! 对应设计文档 §6.2 的 T-I-01 ~ T-I-08（T-I-01 需真实 ollama，#[ignore] 手动验收）。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use assert_cmd::Command;

/// 在本地随机端口起一个最小 ollama mock：接收任意 POST /api/chat，
/// 返回 `{"message": {"content": <content>}}`。
/// 返回 base_url。
fn spawn_ollama_mock(content: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock 端口");
    let addr = listener.local_addr().unwrap();
    let body = serde_json::json!({
        "message": { "role": "assistant", "content": content }
    })
    .to_string();
    let body = body.to_string();
    thread::spawn(move || {
        // 服务若干次连接（Connection: close 使每请求一条连接）
        for stream in listener.incoming().take(16) {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let _ = handle_http(&mut stream, &body);
        }
    });
    format!("http://{addr}")
}

/// 处理一条 HTTP 请求：读 header + Content-Length 定长的 body，回 200。
fn handle_http(stream: &mut std::net::TcpStream, body: &str) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // 读到 header 结束
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return Ok(()),
        }
    }
    let header = String::from_utf8_lossy(&buf).to_string();
    let content_length = header
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut rest = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut rest)?;
    }
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}

/// v0.3 起 CLI 走 daemon-first：每个测试用独立的 XDG 三目录（config / data /
/// cache）+ 独立 socket，互相不串；idle_secs=10 让拉起的 daemon 在测试后自清。
fn write_config(base_url: &str) -> PathBuf {
    let root = tempfile::tempdir().expect("创建临时目录").keep();
    let cfg_dir = root.join("config").join("hlp");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "provider = \"ollama\"\n\n[ollama]\nbase_url = \"{base_url}\"\nmodel = \"mock-model\"\ntimeout_ms = 5000\nmax_candidates = 3\n\n[daemon]\nidle_secs = 10\nsocket_path = \"{}\"\n",
            root.join("daemon.sock").display()
        ),
    )
    .unwrap();
    root
}

/// 便捷封装：以隔离的 XDG 三目录运行 hlp 二进制。
fn hlp(root: &Path) -> Command {
    let mut cmd = Command::cargo_bin("hlp").unwrap();
    cmd.env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"));
    cmd
}

const JSON_TWO_CANDIDATES: &str = r#"{"candidates": ["find . -type f -size +100M", "du -ah . | sort -rh"]}"#;

/// T-I-02：stdout 严格等于 top1 + "\n"（无其他字节、无 ANSI），stderr 含备选提示
#[test]
fn ti02_stdout_purity() {
    let base = spawn_ollama_mock(JSON_TWO_CANDIDATES);
    let xdg = write_config(&base);
    let out = hlp(&xdg)
        .arg("--one-line")
        .arg("列出大文件")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "find . -type f -size +100M\n"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("还有 1 个备选"), "stderr 应含备选提示：{stderr}");
    assert!(!stderr.contains("find"), "stderr 不应包含命令本身");
}

/// T-I-03：--alt 输出全部候选，每行一条纯命令
#[test]
fn ti03_alt_expands_candidates() {
    let base = spawn_ollama_mock(JSON_TWO_CANDIDATES);
    let xdg = write_config(&base);
    let out = hlp(&xdg)
        .arg("--one-line")
        .arg("--alt")
        .arg("列出大文件")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "find . -type f -size +100M\ndu -ah . | sort -rh\n"
    );
}

/// T-I-04：Provider 不可达 → 退出码 4，stdout 为空，stderr 含行动提示
#[test]
fn ti04_provider_unreachable() {
    // 绑定后立刻释放端口，得到一个几乎必然拒绝连接的地址
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let xdg = write_config(&format!("http://{addr}"));
    let out = hlp(&xdg)
        .arg("--one-line")
        .arg("任意意图")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("请确认 ollama 已启动"),
        "stderr 应含行动提示：{stderr}"
    );
}

/// T-I-05：空候选 JSON → 退出码 5，stdout 为空
#[test]
fn ti05_no_candidates() {
    let base = spawn_ollama_mock(r#"{"candidates": []}"#);
    let xdg = write_config(&base);
    let out = hlp(&xdg).arg("--one-line").arg("x").output().unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert!(out.stdout.is_empty());
}

/// T-I-06：非 JSON 纯文本响应 → 兜底解析生效，退出码 0
#[test]
fn ti06_fallback_plain_text() {
    let content = "1. find . -type f -size +100M\n2. du -sh .\n这条命令会查找大文件。";
    let base = spawn_ollama_mock(content);
    let xdg = write_config(&base);
    let out = hlp(&xdg).arg("--one-line").arg("x").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "find . -type f -size +100M\n"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("还有 1 个备选"));
}

/// T-I-07：缺位置参数 → 退出码 2，stdout 为空
#[test]
fn ti07_missing_intent() {
    let xdg = write_config("http://127.0.0.1:11434");
    let out = hlp(&xdg).arg("--one-line").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

/// T-I-08（危险命令不误伤回归）：用户明确要求的 rm -rf 正常翻译，
/// 命令原文不得被篡改、转义或拦截——v0.1 无熔断层，为 v0.2 保留正确 baseline。
#[test]
fn ti08_dangerous_command_not_mangled() {
    let base = spawn_ollama_mock(r#"{"candidates": ["rm -rf /tmp/testdata"]}"#);
    let xdg = write_config(&base);
    let out = hlp(&xdg)
        .arg("--one-line")
        .arg("删除 /tmp/testdata")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "rm -rf /tmp/testdata\n"
    );
}

/// T-I-01：真实 ollama 端到端验收（需本机 ollama 在线且模型已拉取）：
/// `cargo test -- --ignored`
#[test]
#[ignore = "需要本机 ollama 在线且已 pull 模型"]
fn ti01_real_ollama_end_to_end() {
    let out = Command::cargo_bin("hlp")
        .unwrap()
        .arg("--one-line")
        .arg("列出当前目录大文件")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.ends_with('\n') && stdout.lines().count() == 1);
    let cmd = stdout.trim();
    assert!(
        cmd.contains("find") || cmd.contains("du") || cmd.contains("ls"),
        "应生成查找/列目录类命令：{cmd}"
    );
}

/// 补充：多词位置参数拼接（不加引号的意图）
#[test]
fn multi_word_intent_joined() {
    let content = r#"{"candidates": ["ls -l"]}"#;
    let base = spawn_ollama_mock(content);
    let xdg = write_config(&base);
    let out = hlp(&xdg)
        .args(["把", "当前", "目录", "文件", "列出来"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "ls -l\n");
}

/// 补充：--verbose 元信息走 stderr，stdout 仍纯净
#[test]
fn verbose_goes_to_stderr() {
    let base = spawn_ollama_mock(JSON_TWO_CANDIDATES);
    let xdg = write_config(&base);
    let out = hlp(&xdg)
        .arg("--verbose")
        .arg("列出大文件")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "find . -type f -size +100M\n"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("provider=ollama"));
    assert!(stderr.contains("model=mock-model"));
    assert!(stderr.contains("candidates=2"));
}
