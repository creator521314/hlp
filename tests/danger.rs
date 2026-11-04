//! v0.2 集成测试：shell 集成脚本、`--check` 协议、`--trust` 信任表。
//! 对应设计文档 §6.2 的 T-I-11 ~ T-I-18（手动验收 M-1~M-9 见 README）。

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// 空的 XDG_CONFIG_HOME（不预置 config.toml：这三条路径不该读它）
fn empty_xdg() -> PathBuf {
    tempfile::tempdir().expect("创建临时目录").keep()
}

fn hlp(xdg: &Path) -> Command {
    let mut cmd = Command::cargo_bin("hlp").unwrap();
    cmd.env("XDG_CONFIG_HOME", xdg);
    cmd
}

fn danger_file(xdg: &Path) -> PathBuf {
    xdg.join("hlp").join("danger.toml")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// T-I-11：三种 shell 的 init 输出——退出 0，含各自的 Ctrl+G 绑定与 hlp 调用
#[test]
fn ti11_init_outputs_integration_scripts() {
    let xdg = empty_xdg();
    let cases = [
        ("zsh", "bindkey '^G'"),
        ("bash", r#"bind -x '"\C-g": hlp-replace'"#),
        ("fish", r"bind \cg"),
    ];
    for (shell, binding) in cases {
        let out = hlp(&xdg).args(["init", shell]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{shell} init 应退出 0");
        let text = stdout(&out);
        assert!(text.contains(binding), "{shell} 脚本应含 {binding}");
        assert!(text.contains("hlp --one-line"), "{shell} 脚本应调用 --one-line");
        assert!(text.contains("hlp --check"), "{shell} 脚本应调用 --check");
        assert!(stderr(&out).is_empty(), "{shell} init 不应写 stderr");
    }
}

/// T-I-11 补充：非法 shell 名 / 缺参数 → clap 用法错误（退出 2）
#[test]
fn ti11b_init_rejects_bad_shell() {
    let xdg = empty_xdg();
    let out = hlp(&xdg).args(["init", "tcsh"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());

    let out = hlp(&xdg).arg("init").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// T-I-12：--check "rm -rf /" → 退出 11，fatal + rm_recursive_root
#[test]
fn ti12_check_fatal() {
    let xdg = empty_xdg();
    let out = hlp(&xdg).args(["--check", "rm -rf /"]).output().unwrap();
    assert_eq!(out.status.code(), Some(11));
    let text = stdout(&out);
    assert!(text.contains("level=fatal"), "{text}");
    assert!(text.contains("pattern=rm_recursive_root"), "{text}");
    assert!(text.contains("reason="), "{text}");
    assert!(stderr(&out).is_empty(), "判定结果只走 stdout");
}

/// T-I-13：--trust 创建 danger.toml 并写入条目，stdout 为空
#[test]
fn ti13_trust_creates_file() {
    let xdg = empty_xdg();
    let out = hlp(&xdg)
        .args(["--trust", "rm_recursive", "--example", "rm -rf ./build"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "--trust 成功时 stdout 必须为空");
    let text = std::fs::read_to_string(danger_file(&xdg)).unwrap();
    assert!(text.contains("[[trust]]"), "{text}");
    assert!(text.contains("pattern = \"rm_recursive\""), "{text}");
    assert!(text.contains("example = \"rm -rf ./build\""), "{text}");
    assert!(text.contains("trusted_at = "), "{text}");
}

/// T-I-14：先 --trust，再 --check 同模式 → 退出 0，clean + trusted=true
#[test]
fn ti14_trusted_command_is_clean() {
    let xdg = empty_xdg();
    hlp(&xdg)
        .args(["--trust", "rm_recursive"])
        .output()
        .unwrap();
    let out = hlp(&xdg)
        .args(["--check", "rm -rf ./build"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("level=clean"), "{text}");
    assert!(text.contains("pattern=rm_recursive"), "{text}");
    assert!(text.contains("trusted=true"), "{text}");
}

/// T-I-15：--trust 未知 ID → 退出 12，stderr 列出 8 个合法 ID
#[test]
fn ti15_unknown_pattern_exit_12() {
    let xdg = empty_xdg();
    let out = hlp(&xdg)
        .args(["--trust", "no_such_pattern"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(12));
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    for id in [
        "rm_recursive_root",
        "dd_device",
        "mkfs",
        "rm_recursive",
        "redirect_overwrite",
        "chmod_recursive",
        "pipe_to_shell",
        "kill_9_unscoped",
    ] {
        assert!(err.contains(id), "stderr 应列出合法 ID {id}：{err}");
    }
}

/// T-I-16：无命中时 stdout 字节级等于 "level=clean\n"
#[test]
fn ti16_clean_stdout_is_exact() {
    let xdg = empty_xdg();
    let out = hlp(&xdg).args(["--check", "ls -l"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "level=clean\n");
    assert!(out.stderr.is_empty());
}

/// T-I-17：--check 与 --one-line 同用 → 退出 2（互斥校验）
#[test]
fn ti17_check_conflicts_with_one_line() {
    let xdg = empty_xdg();
    let out = hlp(&xdg)
        .args(["--check", "ls -l", "--one-line"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

/// T-I-17 补充：--check / --trust 与位置参数意图互斥；--example 必须配 --trust
#[test]
fn ti17b_flag_intent_mutual_exclusion() {
    let xdg = empty_xdg();
    let out = hlp(&xdg)
        .args(["--check", "ls -l", "列出", "文件"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = hlp(&xdg).args(["--trust", "mkfs", "删除"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = hlp(&xdg).args(["--example", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// T-I-18：危险不误伤回归——/tmp 非系统级，正常清理只走 warning
#[test]
fn ti18_tmp_cleanup_is_warning_not_fatal() {
    let xdg = empty_xdg();
    let out = hlp(&xdg)
        .args(["--check", "rm -rf /tmp/testdata"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    let text = stdout(&out);
    assert!(text.contains("level=warning"), "{text}");
    assert!(text.contains("pattern=rm_recursive\n"), "{text}");
}

/// 补充：danger.toml 损坏 → --check 退出 3，stderr 指明路径
#[test]
fn check_reports_broken_danger_toml() {
    let xdg = empty_xdg();
    std::fs::create_dir_all(xdg.join("hlp")).unwrap();
    std::fs::write(danger_file(&xdg), "this is not toml\n").unwrap();
    let out = hlp(&xdg).args(["--check", "ls"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    assert!(err.contains("danger.toml 无效"), "{err}");
    assert!(err.contains("danger.toml"), "{err}");
}

/// 补充：--trust 幂等——重复调用不改变文件
#[test]
fn trust_is_idempotent_over_cli() {
    let xdg = empty_xdg();
    hlp(&xdg)
        .args(["--trust", "pipe_to_shell", "--example", "curl x | sh"])
        .output()
        .unwrap();
    let first = std::fs::read_to_string(danger_file(&xdg)).unwrap();
    let out = hlp(&xdg)
        .args(["--trust", "pipe_to_shell"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let second = std::fs::read_to_string(danger_file(&xdg)).unwrap();
    assert_eq!(first, second);
}

/// 补充：--check 不读 config.toml（ollama 未配置/不可用也能跑）
#[test]
fn check_does_not_need_provider_config() {
    let xdg = empty_xdg();
    std::fs::create_dir_all(xdg.join("hlp")).unwrap();
    std::fs::write(
        xdg.join("hlp").join("config.toml"),
        "provider = \"ollama\"\n[ollama]\nbase_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let out = hlp(&xdg).args(["--check", "echo hi"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "level=clean\n");
}
