//! daemon 模块门面：拉起与路径（v0.3 设计 §2.3 / §4.1）。

pub mod client;
pub mod protocol;
pub mod server;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::config::Config;
use crate::error::HlpError;

/// socket 路径（§2.2）：配置显式路径 > `$XDG_RUNTIME_DIR/hlp.sock` > `/tmp/hlp-{uid}.sock`。
pub fn socket_path(cfg: &Config) -> PathBuf {
    let configured = cfg.daemon.socket_path.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("hlp.sock"),
        _ => PathBuf::from(format!("/tmp/hlp-{}.sock", uid())),
    }
}

/// 拉起互斥锁：`~/.cache/hlp/daemon.lock`（§3.4，0600）。
pub fn lock_path() -> PathBuf {
    runtime_dir().join("daemon.lock")
}

/// daemon 日志：`~/.cache/hlp/daemon.log`。
pub fn log_path() -> PathBuf {
    runtime_dir().join("daemon.log")
}

fn runtime_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hlp")
}

/// 当前 uid：读 /proc/self 属主（XDG_RUNTIME_DIR 缺失时构造 /tmp/hlp-{uid}.sock）。
fn uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(1_000)
}

/// 确保 daemon 存活（§4.1）：socket 可连且 Ping 通 → 直接返回；
/// 否则持 flock 二次检查后 spawn 自身 `hlp daemon`，轮询 Ping 至就绪。
/// 返回 Err 表示拉起失败，调用方回退直连（决策 5）。
pub fn ensure_running(cfg: &Config) -> Result<(), HlpError> {
    if client::ping(cfg) {
        return Ok(());
    }

    // flock 防并发双拉起：阻塞等锁 ≤ 2s，超时回退
    let dir = runtime_dir();
    fs::create_dir_all(&dir).map_err(|e| {
        HlpError::DaemonStart { reason: format!("无法创建 {}: {e}", dir.display()) }
    })?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path())
        .map_err(|e| HlpError::DaemonStart { reason: format!("打开 lock 失败：{e}") })?;
    let _ = fs::set_permissions(lock_path(), fs::Permissions::from_mode(0o600));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(_) => {
                if Instant::now() >= deadline {
                    return Err(HlpError::DaemonStart {
                        reason: "flock 竞争超时（2s）".to_string(),
                    });
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    // 持锁后二次检查（竞争者可能已拉起）
    if client::ping(cfg) {
        return Ok(());
    }

    // spawn 自身：`hlp daemon`（决策 8：不做 setsid 守护化，随终端会话共生死）
    let exe = std::env::current_exe()
        .map_err(|e| HlpError::DaemonStart { reason: format!("无法定位自身 exe：{e}") })?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .map_err(|e| HlpError::DaemonStart { reason: format!("打开日志失败：{e}") })?;
    let spawned = Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| {
            HlpError::DaemonStart { reason: e.to_string() }
        })?))
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| HlpError::DaemonStart { reason: format!("spawn 失败：{e}") })?;
    // 不等待：daemon 是常驻子进程，父进程（CLI）随即退出后由 init 接管
    drop(spawned);

    // 轮询就绪：10ms × 100 = 1s 上限
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if client::ping(cfg) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(HlpError::DaemonStart {
                reason: "daemon 1s 内未就绪".to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
