//! v0.3 集成测试：daemon 生命周期、流式、缓存学习闭环。
//! 对应设计文档 §6.2 的 T-I-20 ~ T-I-31。
//!
//! 隔离：每个测试独立 XDG 三目录（config/data/cache）+ 独立 socket + 独立 mock ollama。
//! mock 的响应脚本（script.txt）每行 = 一个流式 chunk 的 content 片段；
//! 非流式请求把全部行拼接为完整 content。requests.log 记录请求次数。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

/// 测试环境：隔离根目录 + mock ollama + 指向 mock 的 config.toml。
struct TestEnv {
    root: PathBuf,
    base_url: String,
    script: PathBuf,
    requests: PathBuf,
    socket: PathBuf,
}

impl TestEnv {
    fn new(tag: &str) -> Self {
        Self::with_extra(tag, "")
    }

    /// extra 会原样追加到 config.toml（供 [cache] 豁免等测试定制）。
    fn with_extra(_tag: &str, extra: &str) -> Self {
        let root = tempfile::tempdir().expect("创建临时目录").keep();
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock 端口");
        let addr = listener.local_addr().unwrap();
        let script = root.join("script.txt");
        let requests = root.join("requests.log");
        std::fs::write(&script, "{\"candidates\": [\"ls -la\"]}\n").unwrap();

        let (s_clone, r_clone) = (script.clone(), requests.clone());
        thread::spawn(move || {
            // 服务若干次连接（Connection: close 使每请求一条连接）
            for st in listener.incoming().take(64).flatten() {
                let _ = handle_mock(st, &s_clone, &r_clone);
            }
        });

        let env = TestEnv {
            root: root.clone(),
            base_url: format!("http://{addr}"),
            script,
            requests,
            socket: root.join("daemon.sock"),
        };
        env.write_config(extra);
        env
    }

    fn write_config(&self, extra: &str) {
        let cfg_dir = self.root.join("config").join("hlp");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            format!(
                "provider = \"ollama\"\n\n[ollama]\nbase_url = \"{}\"\nmodel = \"mock-model\"\ntimeout_ms = 5000\nmax_candidates = 3\n\n[daemon]\nidle_secs = 10\nsocket_path = \"{}\"\n{extra}\n",
                self.base_url,
                self.socket.display()
            ),
        )
        .unwrap();
    }

    /// 设置 mock 响应脚本：每行 = 一个流式 chunk。
    fn set_script(&self, lines: &[&str]) {
        let mut s = lines.join("\n");
        s.push('\n');
        std::fs::write(&self.script, s).unwrap();
    }

    fn request_count(&self) -> usize {
        std::fs::read_to_string(&self.requests)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    /// 跑一次 hlp 并等待结束。
    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hlp"));
        c.args(args)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"));
        c
    }
}

/// 直接说 socket 协议的健康检查（测试侧手写 NDJSON，顺带覆盖协议互操作）。
fn ping(socket: &Path) -> bool {
    let Ok(mut s) = UnixStream::connect(socket) else {
        return false;
    };
    if s
        .write_all(b"{\"v\":1,\"op\":\"ping\"}\n")
        .and_then(|_| s.flush())
        .is_err()
    {
        return false;
    }
    let mut line = String::new();
    match BufReader::new(s).read_line(&mut line) {
        Ok(n) if n > 0 => line.contains("\"ok\""),
        _ => false,
    }
}

fn wait_until(deadline_ms: u64, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// mock ollama：读 header+body；`"stream":true` → 逐行 NDJSON chunk，
/// 否则单 JSON 响应（content = 全部行拼接）。
fn handle_mock(mut stream: std::net::TcpStream, script: &Path, requests: &Path) -> std::io::Result<()> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte)? {
            0 => return Ok(()),
            _ => {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let head = String::from_utf8_lossy(&header).to_string();
    let content_length = head
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
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(requests)?
        .write_all(b"req\n")?;

    let script_text = std::fs::read_to_string(script).unwrap_or_default();
    let lines: Vec<&str> = script_text.lines().filter(|l| !l.is_empty()).collect();

    if String::from_utf8_lossy(&body).contains("\"stream\":true") {
        // 流式：每行一个 chunk，末尾 done:true（无 Content-Length，靠 Connection: close 终结）
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
        )?;
        for line in &lines {
            let content = serde_json::json!({ "message": { "role": "assistant", "content": line }, "done": false });
            let mut frame = content.to_string();
            frame.push('\n');
            stream.write_all(frame.as_bytes())?;
            stream.flush()?;
        }
        let done = serde_json::json!({ "message": { "role": "assistant", "content": "" }, "done": true });
        let mut frame = done.to_string();
        frame.push('\n');
        stream.write_all(frame.as_bytes())?;
    } else {
        let content: String = lines.concat();
        let resp = serde_json::json!({ "message": { "role": "assistant", "content": content } }).to_string();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            resp.len()
        );
        stream.write_all(header.as_bytes())?;
        stream.write_all(resp.as_bytes())?;
    }
    stream.flush()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// T-I-20：daemon 拉起后 Ping 通；第二个 daemon 让位退出 0
#[test]
fn ti20_daemon_spawn_and_yield() {
    let env = TestEnv::new("ti20");
    let mut child = env.cmd(&["daemon"]).spawn().unwrap();
    assert!(
        wait_until(5_000, || ping(&env.socket)),
        "daemon 应在 5s 内就绪"
    );

    // 让位：已有存活实例 → 退出码 0（非错误，遗留风险 #5）
    let second = env.run(&["daemon"]);
    assert_eq!(second.status.code(), Some(0), "第二个 daemon 应让位退出 0");
    assert!(second.stdout.is_empty());
    // 原实例不受影响
    assert!(ping(&env.socket));

    child.kill().unwrap();
    child.wait().unwrap();
}

/// T-I-21：idle_secs=10，无请求静默 → 进程退出码 0，socket 文件被清理
#[test]
fn ti21_idle_exit_cleans_socket() {
    let env = TestEnv::new("ti21");
    let mut child = env.cmd(&["daemon"]).spawn().unwrap();
    assert!(wait_until(5_000, || ping(&env.socket)));

    // idle_secs 校验下限是 10s：等它自然退出（宽限 15s）
    let exited = wait_until(15_000, || child.try_wait().map(|s| s.is_some()).unwrap_or(false));
    assert!(exited, "daemon 应在空闲后退出");
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "空闲退出码应为 0");
    assert!(!env.socket.exists(), "socket 文件应被清理");
}

/// T-I-22：并发两个客户端 → flock 保证单实例，两者都拿到结果
#[test]
fn ti22_concurrent_clients_single_daemon() {
    let env = TestEnv::new("ti22");
    env.set_script(&["{\"candidates\": [\"ls -la\"]}"]);

    use std::process::Stdio;
    let a = env
        .cmd(&["--one-line", "列出文件"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let b = env
        .cmd(&["--one-line", "列出文件"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let oa = a.wait_with_output().unwrap();
    let ob = b.wait_with_output().unwrap();
    assert_eq!(
        stdout_of(&oa),
        "ls -la\n",
        "a 失败：rc={:?}\nstderr={}",
        oa.status.code(),
        stderr_of(&oa)
    );
    assert_eq!(
        stdout_of(&ob),
        "ls -la\n",
        "b 失败：rc={:?}\nstderr={}",
        ob.status.code(),
        stderr_of(&ob)
    );
    assert!(oa.status.success() && ob.status.success());

    // 只有一个 daemon 实例的佐证：再来一个 hlp daemon 让位（flock 生效）
    let third = env.run(&["daemon"]);
    assert_eq!(third.status.code(), Some(0));
    assert!(ping(&env.socket));
}

/// T-I-23：--stream 经 daemon：stdout 多行快照递增，末行 = 最终 top1
#[test]
fn ti23_stream_snapshots_via_daemon() {
    let env = TestEnv::new("ti23");
    env.set_script(&["{\"candidates\": [\"find . -ty", "pe f -size +100M\"]}"]);
    let out = env.run(&["--stream", "列出大文件"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["find . -ty", "find . -type f -size +100M"]);
    assert!(stdout.ends_with("find . -type f -size +100M\n"), "末行应为最终 top1");
}

/// T-I-24：learn 后同意图 → source=cache，端到端 < 1s，mock ollama 零请求
#[test]
fn ti24_cache_hit_sub_second_no_llm() {
    let env = TestEnv::new("ti24");
    env.set_script(&["{\"candidates\": [\"du -ah .\"]}"]); // 若被请求，结果必然不同 → 断言失败

    let learn = env.run(&[
        "cache-learn",
        "--intent",
        "列出大文件",
        "--command",
        "find . -size +100M",
        "--source",
        "adopted",
    ]);
    assert_eq!(learn.status.code(), Some(0));
    assert!(learn.stdout.is_empty());

    let start = Instant::now();
    let out = env.run(&["--verbose", "--one-line", "列出大文件"]);
    let elapsed = start.elapsed();
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "find . -size +100M\n");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("source=cache"), "verbose 应标注 source=cache：{stderr}");
    assert!(elapsed < Duration::from_secs(1), "缓存命中应亚秒，实际 {elapsed:?}");
    assert_eq!(env.request_count(), 0, "缓存命中不得请求 LLM");
}

/// T-I-25：cache-learn adopted → --one-line 命中缓存
#[test]
fn ti25_cache_learn_adopted_hit() {
    let env = TestEnv::new("ti25");
    let learn = env.run(&["cache-learn", "--intent", "a", "--command", "pwd"]);
    assert_eq!(learn.status.code(), Some(0));

    let out = env.run(&["--one-line", "a"]);
    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "pwd\n");
    assert_eq!(env.request_count(), 0);
}

/// T-I-26：先 adopted 再 correction → 缓存返回修正后的命令
#[test]
fn ti26_correction_overwrites() {
    let env = TestEnv::new("ti26");
    env.run(&["cache-learn", "--intent", "a", "--command", "echo a"]);
    env.run(&[
        "cache-learn",
        "--intent",
        "a",
        "--command",
        "echo b",
        "--source",
        "correction",
    ]);
    let out = env.run(&["--one-line", "a"]);
    assert_eq!(stdout_of(&out), "echo b\n");
    let stderr = stderr_of(&out);
    assert!(stderr.is_empty() || !stderr.contains("还有"), "单候选无备选提示：{stderr}");
}

/// T-I-27：--clear 清空历史，之后不再命中（走 LLM）
#[test]
fn ti27_clear_empties_cache() {
    let env = TestEnv::new("ti27");
    env.run(&["cache-learn", "--intent", "a", "--command", "echo learned"]);

    let clear = env.run(&["--clear"]);
    assert_eq!(clear.status.code(), Some(0));
    assert!(clear.stdout.is_empty());

    env.set_script(&["{\"candidates\": [\"ls -la\"]}"]);
    let out = env.run(&["--one-line", "a"]);
    assert_eq!(stdout_of(&out), "ls -la\n", "clear 后应回退 LLM");
    assert_eq!(env.request_count(), 1);
}

/// T-I-28：exempt_dirs 命中的 cwd 下 cache-learn 不落库（静默退出 0）
#[test]
fn ti28_exempt_dir_not_recorded() {
    let exempt = tempfile::tempdir().unwrap().keep();
    let extra = format!("\n[cache]\nexempt_dirs = [\"{}\"]\n", exempt.display());
    let env = TestEnv::with_extra("ti28", &extra);
    env.set_script(&["{\"candidates\": [\"ls -la\"]}"]);

    let learn = env.run(&[
        "cache-learn",
        "--intent",
        "a",
        "--command",
        "echo secret",
        "--cwd",
        exempt.to_str().unwrap(),
    ]);
    assert_eq!(learn.status.code(), Some(0), "豁免目录下应静默成功");

    // 无记录 → 回退 LLM，返回 mock 的答案
    let out = env.run(&["--one-line", "a"]);
    assert_eq!(stdout_of(&out), "ls -la\n");
    assert_eq!(env.request_count(), 1);
}

/// T-I-29：daemon 崩溃（被杀）→ 客户端自愈：respawn 或直连，stdout 只有完整快照行
#[test]
fn ti29_daemon_death_self_heals() {
    let env = TestEnv::new("ti29");
    env.set_script(&["{\"candidates\": [\"find . -ty", "pe f -size +100M\"]}"]);

    let mut child = env.cmd(&["daemon"]).spawn().unwrap();
    assert!(wait_until(5_000, || ping(&env.socket)));
    child.kill().unwrap();
    child.wait().unwrap();
    // socket 文件残留（kill 不清理）——ensure_running 应检测到死实例并自愈

    let out = env.run(&["--stream", "列出大文件"]);
    let code = out.status.code().unwrap();
    assert!(
        code == 0 || code == 4,
        "回退直连成功（0）或服务错误（4），实际 {code}"
    );
    if code == 0 {
        let stdout = stdout_of(&out);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines, vec!["find . -ty", "find . -type f -size +100M"], "无脏快照行");
    }
}

/// T-I-31：init v2 模板——流式调用 + preexec 学习钩子
#[test]
fn ti31_init_v2_templates() {
    let env = TestEnv::new("ti31");

    let zsh = env.run(&["init", "zsh"]);
    assert_eq!(zsh.status.code(), Some(0));
    let zsh = stdout_of(&zsh);
    assert!(zsh.contains("hlp --stream"), "zsh v2 应走 --stream");
    assert!(zsh.contains("add-zsh-hook preexec"), "zsh v2 应有 preexec 钩子");

    let bash = stdout_of(&env.run(&["init", "bash"]));
    assert!(bash.contains("trap 'hlp-preexec' DEBUG"), "bash v2 应有 DEBUG trap");

    let fish = stdout_of(&env.run(&["init", "fish"]));
    assert!(fish.contains("fish_preexec"), "fish v2 应有 fish_preexec 钩子");
    assert!(fish.contains("hlp --stream"));
}

/// 补充：cache-learn 参数缺失 → 退出 2（clap）
#[test]
fn cache_learn_requires_args() {
    let env = TestEnv::new("learn-args");
    let out = env.run(&["cache-learn"]);
    assert_eq!(out.status.code(), Some(2));
    let out = env.run(&["cache-learn", "--intent", "a"]); // 缺 --command
    assert_eq!(out.status.code(), Some(2));
}

/// 补充：--stream 与 --alt 互斥 → 退出 2
#[test]
fn stream_conflicts_with_alt() {
    let env = TestEnv::new("stream-conflict");
    let out = env.run(&["--stream", "--alt", "x"]);
    assert_eq!(out.status.code(), Some(2));
}

/// 补充：--clear 与意图参数互斥 → 退出 2
#[test]
fn clear_conflicts_with_intent() {
    let env = TestEnv::new("clear-conflict");
    let out = env.run(&["--clear", "列出文件"]);
    assert_eq!(out.status.code(), Some(2));
}

/// 补充：学习来源仅接受 adopted / correction
#[test]
fn cache_learn_rejects_bad_source() {
    let env = TestEnv::new("bad-source");
    let out = env.run(&[
        "cache-learn",
        "--intent",
        "a",
        "--command",
        "c",
        "--source",
        "nope",
    ]);
    assert_eq!(out.status.code(), Some(2));
}