//! v0.4 集成测试：知识库三层链、解释模式、dry-run、TUI 非 TTY、模板回归。
//! 对应设计文档 §6.2 的 T-I-32 ~ T-I-44。
//!
//! 隔离：每个测试独立 XDG 三目录 + 独立 socket + 独立 mock ollama +
//! 内嵌 fixture tldr.zip（HLP_KB_URL=file://…）。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Instant;

const FIND_ZH: &str = "# find\n\n> 查找文件。\n> More information: <https://example.com>.\n\n- 列出大文件：\n\n`find {{路径}} -size +100M`\n";
const SORT_EN: &str = "# sort\n\n> Sort lines.\n\n- Sort numerically:\n\n`sort -n`\n";
const WC_ZH: &str = "# wc\n\n> 统计行数。\n\n- 统计行数：\n\n`wc -l {{文件}}`\n";

struct Env {
    root: PathBuf,
    base_url: String,
    script: PathBuf,
    requests: PathBuf,
    socket: PathBuf,
    fixture: PathBuf,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("临时目录").keep();
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock 端口");
        let addr = listener.local_addr().unwrap();
        let script = root.join("script.txt");
        let requests = root.join("requests.log");
        fs::write(&script, "{\"candidates\": [\"ls -la\"]}\n").unwrap();
        fs::write(&requests, "").unwrap();

        let fixture = root.join("fixture.zip");
        write_fixture_zip(&fixture);

        let (s, r) = (script.clone(), requests.clone());
        thread::spawn(move || {
            for stream in listener.incoming().take(128).flatten() {
                let _ = handle_mock(stream, &s, &r);
            }
        });

        let env = Env {
            root: root.clone(),
            base_url: format!("http://{addr}"),
            script,
            requests,
            socket: root.join("daemon.sock"),
            fixture,
        };
        env.write_config("");
        env
    }

    fn write_config(&self, extra: &str) {
        let cfg_dir = self.root.join("config").join("hlp");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::write(
            cfg_dir.join("config.toml"),
            format!(
                "provider = \"ollama\"\n\n[ollama]\nbase_url = \"{}\"\nmodel = \"mock-model\"\ntimeout_ms = 5000\nmax_candidates = 3\n\n[daemon]\nidle_secs = 10\nsocket_path = \"{}\"\n\n[kb]\nenabled = true\nmin_score = 0.6\n\n[explain]\nlanguage = \"auto\"\n{extra}\n",
                self.base_url,
                self.socket.display()
            ),
        )
        .unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hlp"));
        c.args(args)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("LANG", "zh_CN.UTF-8")
            .env("LC_ALL", "zh_CN.UTF-8")
            .env("HLP_KB_URL", format!("file://{}", self.fixture.display()));
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn nreq(&self) -> usize {
        fs::read_to_string(&self.requests)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn set_script(&self, lines: &[&str]) {
        let mut s = lines.join("\n");
        s.push('\n');
        fs::write(&self.script, s).unwrap();
    }

    /// 执行 kb-update（3 页 fixture），返回输出。
    fn kb_update(&self) -> Output {
        self.run(&["kb-update"])
    }
}

fn write_fixture_zip(path: &Path) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        ("pages.zh/common/find.md", FIND_ZH),
        ("pages/common/sort.md", SORT_EN),
        ("pages.zh/common/wc.md", WC_ZH),
    ] {
        zip.start_file(name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

/// mock ollama：explain 请求（system 含"解释器"）→ segments JSON；
/// 否则按 stream 标志返回 NDJSON 或单 JSON（内容来自 script.txt）。
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
    let len = head
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
    let mut body = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut body)?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(requests)?
        .write_all(b"req\n")?;

    let is_explain = String::from_utf8_lossy(&body).contains("解释器");
    let is_stream = String::from_utf8_lossy(&body).contains("\"stream\":true");
    let script_text = fs::read_to_string(script).unwrap_or_default();
    let lines: Vec<&str> = script_text.lines().filter(|l| !l.is_empty()).collect();

    if is_explain {
        let content = serde_json::json!({
            "segments": [
                {"cmd": "find . -name '*.jpg'", "note": "递归查找 jpg 文件"},
                {"cmd": "wc -l", "note": "统计结果行数"}
            ]
        })
        .to_string();
        let resp = serde_json::json!({"message": {"role": "assistant", "content": content}}).to_string();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            resp.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(resp.as_bytes())?;
    } else if is_stream {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
        )?;
        for line in &lines {
            let frame = serde_json::json!({
                "message": {"role": "assistant", "content": line},
                "done": false
            })
            .to_string();
            stream.write_all(format!("{frame}\n").as_bytes())?;
            stream.flush()?;
        }
        let done = serde_json::json!({
            "message": {"role": "assistant", "content": ""},
            "done": true
        })
        .to_string();
        stream.write_all(format!("{done}\n").as_bytes())?;
    } else {
        let content: String = lines.concat();
        let resp = serde_json::json!({"message": {"role": "assistant", "content": content}}).to_string();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            resp.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(resp.as_bytes())?;
    }
    stream.flush()
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// T-I-32：kb-update → kb-status，3 页 / 3 示例；二次更新幂等
#[test]
fn ti32_kb_update_and_status_idempotent() {
    let env = Env::new();
    let out = env.kb_update();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("已导入 3 页 / 3 条示例"), "{}", stderr(&out));

    let status = env.run(&["kb-status"]);
    assert_eq!(status.status.code(), Some(0));
    let text = stdout(&status);
    assert!(text.contains("pages=3"), "{text}");
    assert!(text.contains("examples=3"), "{text}");
    assert!(text.contains("updated_at="), "{text}");

    // 二次更新：DELETE + INSERT 同事务 → 计数不变
    let out2 = env.kb_update();
    assert_eq!(out2.status.code(), Some(0));
    let text = stdout(&env.run(&["kb-status"]));
    assert!(text.contains("pages=3") && text.contains("examples=3"), "{text}");
}

/// T-I-33：kb 高分命中 → stdout 单行、mock 零请求、端到端 < 1s
#[test]
fn ti33_kb_hit_short_circuits_llm() {
    let env = Env::new();
    env.kb_update();
    let before = env.nreq();
    let t0 = Instant::now();
    let out = env.run(&["--one-line", "列出大文件"]);
    let ms = t0.elapsed().as_millis();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "find {{路径}} -size +100M\n");
    assert_eq!(env.nreq(), before, "kb 命中不该请求 LLM");
    assert!(ms < 1000, "kb 命中应亚秒，实测 {ms}ms");
}

/// T-I-33 补充 / T-I-41：冷启动红线 < 200ms、stdout 纯净单行
#[test]
fn ti41_kb_hit_cold_start_budget() {
    let env = Env::new();
    env.kb_update();
    let t0 = Instant::now();
    let out = env.run(&["--one-line", "列出大文件"]);
    let ms = t0.elapsed().as_millis();
    assert!(ms < 200, "进程启动 + kb 命中应 < 200ms，实测 {ms}ms");
    assert_eq!(stdout(&out).lines().count(), 1, "stdout 必须纯净单行");
    assert!(!stdout(&out).contains("source="), "装饰不得进 stdout");
}

/// T-I-43：占位符原样输出
#[test]
fn ti43_placeholder_is_preserved() {
    let env = Env::new();
    env.kb_update();
    let out = env.run(&["--one-line", "列出大文件"]);
    assert!(stdout(&out).contains("{{路径}}"), "{}", stdout(&out));
}

/// T-I-34：语料无关 → 落第 3 层，mock 被请求，stdout 为 LLM top1
#[test]
fn ti34_unrelated_intent_falls_to_llm() {
    let env = Env::new();
    env.kb_update();
    let before = env.nreq();
    let out = env.run(&["--one-line", "查看当前天气"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "ls -la\n");
    assert!(env.nreq() > before, "未命中 kb 应落到 LLM");
}

/// T-I-35：-x 经 LLM 综合，--json 输出 source=llm / lang=zh
#[test]
fn ti35_explain_llm_and_json() {
    let env = Env::new();
    env.kb_update();
    let cmd = "find . -name '*.jpg' | wc -l";
    let out = env.run(&["-x", cmd]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("递归查找 jpg 文件"), "{text}");
    assert!(text.contains("统计结果行数"), "{text}");
    assert!(text.lines().count() >= 4, "首行命令 + 空行 + 两行段：{text}");

    let js = env.run(&["-x", cmd, "--json"]);
    assert_eq!(js.status.code(), Some(0), "stderr: {}", stderr(&js));
    let text = stdout(&js);
    assert!(text.contains("\"source\":\"llm\""), "{text}");
    assert!(text.contains("\"lang\":\"zh\""), "{text}");
    assert!(text.contains("\"segments\""), "{text}");
}

/// T-I-36：--offline 只用本地标注，mock 零请求；无 kb 页的段标注"（无本地注解）"
#[test]
fn ti36_explain_offline_local_only() {
    let env = Env::new();
    env.kb_update();
    let before = env.nreq();
    let out = env.run(&["-x", "find . | uniq -c", "--offline"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("查找文件"), "有 kb 页的段应有解释：{text}");
    assert!(text.contains("（无本地注解）"), "无 kb 页的段应标注：{text}");
    assert_eq!(env.nreq(), before, "--offline 不该请求 LLM");
}

/// T-I-37：v1 协议 daemon 桩 → explain 回退直连成功，退出 0
#[test]
fn ti37_v1_daemon_falls_back_to_direct() {
    let env = Env::new();
    env.kb_update();
    let _ = fs::remove_file(&env.socket);
    let listener = UnixListener::bind(&env.socket).unwrap();
    thread::spawn(move || {
        for s in listener.incoming().take(8).flatten() {
            let mut s = s;
            let _ = s.write_all(
                b"{\"type\":\"error\",\"kind\":\"protocol\",\"message\":\"daemon v1\"}\n",
            );
        }
    });
    let before = env.nreq();
    let out = env.run(&["-x", "find . | sort"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("递归查找 jpg 文件"), "{}", stdout(&out));
    assert!(env.nreq() > before, "应回退直连 Provider");
}

/// T-I-38：dry-run 只读预览，原目录仍存在
#[test]
fn ti38_dry_run_never_executes_original() {
    let env = Env::new();
    let dir = env.root.join("dry-target");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("keep.txt"), "x").unwrap();
    let cmd = format!("rm -rf {}", dir.display());
    let out = env.run(&["--dry-run", &cmd]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("预览（未执行原命令"), "{text}");
    assert!(text.contains("dry-target"), "{text}");
    assert!(dir.join("keep.txt").exists(), "原命令绝不能被执行");
}

/// T-I-39：fatal 类 dry-run → 退出 0、stdout 空、stderr 含"不予预览"
#[test]
fn ti39_dry_run_fatal_unsupported() {
    let env = Env::new();
    let out = env.run(&["--dry-run", "dd if=a of=/dev/sda"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).contains("不予预览"), "{}", stderr(&out));
}

/// T-I-40：非 TTY → 退出码 6，stderr 提示
#[test]
fn ti40_tui_requires_tty() {
    let env = Env::new();
    let out = env.run(&["--tui", "列出大文件"]);
    assert_eq!(out.status.code(), Some(6), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("TUI 不可用"), "{}", stderr(&out));
    assert!(stderr(&out).contains("--one-line"), "{}", stderr(&out));
}

/// T-I-42：shell 集成模板不回归（TUI 不进 shell 集成层）
#[test]
fn ti42_shell_templates_unchanged() {
    let env = Env::new();
    let out = env.run(&["init", "zsh"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("hlp --stream"), "{text}");
    assert!(text.contains("cache-learn"), "{text}");
    assert!(!text.contains("--tui"), "TUI 不得污染 shell 模板：{text}");
}

/// 补充：kb 未命中时 `--stream` 仍走 v0.3 流式快照（多 chunk 递增，末行 = top1）
#[test]
fn stream_snapshots_still_work_on_llm_fallback() {
    let env = Env::new();
    env.kb_update();
    env.set_script(&["{\"candidates\": [\"ls", " -la\"]}"]);
    let out = env.run(&["--stream", "查看当前天气"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 2, "{text}");
    assert_eq!(lines.last().copied(), Some("ls -la"), "末行应为 top1：{text}");
}

/// 补充：CLI 互斥校验（-x 与位置意图、--stream 与 --alt 等）
#[test]
fn cli_v04_mutual_exclusions() {
    let env = Env::new();
    let out = env.run(&["-x", "ls", "多余意图"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = env.run(&["--dry-run", "ls -l", "--tui"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = env.run(&["--offline"]);
    assert_eq!(out.status.code(), Some(2), "--offline 需要 -x：{}", stderr(&out));
}
