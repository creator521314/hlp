//! dry-run：把命令变换为**只读预览命令**并受限执行（v0.4 设计 §2.8）。
//!
//! 安全核心：永远不执行原命令；一律 argv 直执（无 shell、无 glob 展开），
//! 参数含特殊字符即拒绝（决策 9）。变换表内置代码，不开放配置（决策 10）。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::HlpError;
use crate::explain::split::{lex_tokens, split_pipeline};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryPlan {
    /// argv 直执（不经 shell）：程序 + 参数
    Exec {
        program: String,
        args: Vec<String>,
        note: String,
    },
    /// 该命令不支持 dry-run（含永不支持的 fatal 类）
    Unsupported { reason: String },
}

/// 仅支持单段命令（含管道 → Unsupported）。
pub fn plan(command: &str) -> DryPlan {
    if split_pipeline(command).len() > 1 {
        return unsupported("含管道，不予预览（仅支持单段命令）");
    }
    let tokens = lex_tokens(command);
    if tokens.is_empty() {
        return unsupported("命令为空");
    }
    // 注入防护：任何 token 含这些字符 → 拒绝（不经 shell，也不冒命令替换风险）
    const FORBIDDEN: [char; 7] = [';', '&', '`', '$', '<', '>', '\n'];
    if tokens
        .iter()
        .any(|t| t.chars().any(|c| FORBIDDEN.contains(&c)))
    {
        return unsupported("含特殊字符，不予预览");
    }

    let base = tokens[0].rsplit('/').next().unwrap_or(&tokens[0]);
    let rest: Vec<String> = tokens[1..].to_vec();
    match base {
        "dd" | "shutdown" | "reboot" => unsupported("fatal 级命令不予预览"),
        _ if base.starts_with("mkfs") => unsupported("fatal 级命令不予预览"),
        "rm" | "rmdir" => exec(
            "ls",
            with_prefix(&["-ld", "--"], &rest),
            "列出将被删除/移除的对象",
        ),
        "rsync" => {
            let mut argv = rest.clone();
            argv.push("-n".to_string());
            exec("rsync", argv, "rsync 原生 dry-run（-n）")
        }
        "git" if rest.first().map(String::as_str) == Some("clean") => {
            let mut argv = rest.clone();
            argv.push("-n".to_string());
            exec("git", argv, "git clean 原生 dry-run（-n）")
        }
        "mv" | "cp" => {
            // 末位参数是目标，源为末位之前的全部
            let sources: Vec<String> = if rest.len() > 1 {
                rest[..rest.len() - 1].to_vec()
            } else {
                rest.clone()
            };
            exec(
                "ls",
                with_prefix(&["-ld", "--"], &sources),
                "列出将被移动/复制的源对象",
            )
        }
        _ => unsupported("该命令没有内置 dry-run 变换，不予预览"),
    }
}

fn exec(program: &str, args: Vec<String>, note: &str) -> DryPlan {
    DryPlan::Exec {
        program: program.to_string(),
        args,
        note: note.to_string(),
    }
}

fn with_prefix(prefix: &[&str], rest: &[String]) -> Vec<String> {
    let mut v: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    v.extend(rest.iter().cloned());
    v
}

fn unsupported(reason: &str) -> DryPlan {
    DryPlan::Unsupported {
        reason: reason.to_string(),
    }
}

/// 执行 DryPlan::Exec：argv 直执，捕获 stdout/stderr 各截断 4KB，超时 5s 强杀。
/// 执行失败也返回文本（不上升为进程错误，设计 §2.8）。
pub fn run(plan: &DryPlan) -> Result<String, HlpError> {
    let DryPlan::Exec { program, args, note } = plan else {
        return Ok(String::new());
    };
    let mut preview = format!("预览（未执行原命令，{note}）：{program} {}\n", args.join(" "));
    if args.iter().any(|a| a.contains('*') || a.contains('?')) {
        preview.push_str("（glob 未展开：预览仅显示字面量，实际影响范围可能更大）\n");
    }
    let spawned = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            preview.push_str(&format!("（预览命令启动失败：{e}）\n"));
            return Ok(preview);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break false,
        }
    };
    match child.wait_with_output() {
        Ok(out) => {
            if timed_out {
                preview.push_str("（预览命令超时 5s，已终止）\n");
            }
            preview.push_str(&truncate(&String::from_utf8_lossy(&out.stdout)));
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                preview.push_str(&truncate(&err));
            }
        }
        Err(e) => preview.push_str(&format!("（预览命令读取失败：{e}）\n")),
    }
    Ok(preview)
}

fn truncate(s: &str) -> String {
    let mut out: String = s.chars().take(4096).collect();
    if s.chars().count() > 4096 {
        out.push_str("\n…（输出已截断）\n");
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-58：rm → ls -ld -- 原参数
    #[test]
    fn tu58_rm_becomes_ls() {
        assert_eq!(
            plan("rm -rf /tmp/x"),
            DryPlan::Exec {
                program: "ls".to_string(),
                args: vec!["-ld".into(), "--".into(), "-rf".into(), "/tmp/x".into()],
                note: "列出将被删除/移除的对象".to_string(),
            }
        );
    }

    /// T-U-59：注入字符拒绝
    #[test]
    fn tu59_special_chars_rejected() {
        assert!(matches!(plan("rm -rf /tmp/x; echo hi"), DryPlan::Unsupported { .. }));
        assert!(matches!(plan("rm $(date)"), DryPlan::Unsupported { .. }));
        assert!(matches!(plan("ls > /tmp/x"), DryPlan::Unsupported { .. }));
    }

    /// T-U-60：fatal 类与管道拒绝
    #[test]
    fn tu60_fatal_and_pipeline_unsupported() {
        assert!(matches!(plan("dd if=a of=/dev/sda"), DryPlan::Unsupported { .. }));
        assert!(matches!(plan("mkfs.ext4 /dev/sda"), DryPlan::Unsupported { .. }));
        assert!(matches!(plan("a | b"), DryPlan::Unsupported { .. }));
    }

    #[test]
    fn native_dryrun_flags() {
        assert_eq!(
            plan("rsync -a src/ dst/"),
            DryPlan::Exec {
                program: "rsync".into(),
                args: vec!["-a".into(), "src/".into(), "dst/".into(), "-n".into()],
                note: "rsync 原生 dry-run（-n）".into(),
            }
        );
        let p = plan("git clean -fd");
        assert!(matches!(p, DryPlan::Exec { ref program, ref args, .. } if program == "git" && args.last() == Some(&"-n".to_string())));
    }

    #[test]
    fn run_never_executes_original_command() {
        let dir = tempfile::tempdir().unwrap().keep();
        let path = dir.join("keep-me");
        std::fs::write(&path, "x").unwrap();
        let p = plan(&format!("rm -rf {}", path.display()));
        let text = run(&p).unwrap();
        assert!(text.contains("预览（未执行原命令"));
        assert!(path.exists(), "原命令绝不能被执行");
    }
}
