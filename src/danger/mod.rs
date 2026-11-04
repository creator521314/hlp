//! 危险命令分级判定（设计 docs/design/v0.2-shell-integration.md §2.1 / §3.1 / §4.2）。
//!
//! 纯本地、无网络、无 LLM：`--check` 的全部逻辑都在这里，预算 < 10ms。
//! 本模块不执行、不改写任何命令——只回答"这条命令有多危险"（§7.3）。

pub mod patterns;
pub mod trust;

use trust::TrustList;

/// 危险等级。声明顺序即严重程度（派生 Ord），便于断言与取最高级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DangerLevel {
    Clean,
    Info,
    Warning,
    Fatal,
}

impl DangerLevel {
    /// `--check` stdout 协议中的 level 值（设计 §3.2），小写、稳定、不得漂移。
    pub fn as_str(self) -> &'static str {
        match self {
            DangerLevel::Clean => "clean",
            DangerLevel::Info => "info",
            DangerLevel::Warning => "warning",
            DangerLevel::Fatal => "fatal",
        }
    }

    /// `--check` 的退出码（设计 §5）：0 / 10 / 11，只在本模式内使用。
    pub fn exit_code(self) -> i32 {
        match self {
            DangerLevel::Clean | DangerLevel::Info => 0,
            DangerLevel::Warning => 10,
            DangerLevel::Fatal => 11,
        }
    }
}

/// 一次判定的完整结论，供 `--check` 渲染 key=value 与 shell widget 决策。
#[derive(Debug)]
pub struct DangerVerdict {
    /// 信任命中时折叠为 Clean
    pub level: DangerLevel,
    /// 命中规则 ID；Clean 且未命中任何规则时为 None
    pub pattern: Option<&'static str>,
    /// 人话原因（≤ 80 字符）
    pub reason: String,
    /// 命中规则但已在信任列表中
    pub trusted: bool,
}

impl DangerVerdict {
    fn clean() -> Self {
        Self {
            level: DangerLevel::Clean,
            pattern: None,
            reason: String::new(),
            trusted: false,
        }
    }
}

/// 对命令做危险检查。纯函数（信任表由调用方加载后传入，IO 不进入本函数）。
///
/// 规则表按级别降序排列，首条命中即返回——这就是"多级命中取最高级"的实现
/// （`rm -rf / > out` → fatal + rm_recursive_root）。
pub fn check(command: &str, trust: &TrustList) -> DangerVerdict {
    for rule in patterns::builtin_rules() {
        let Some(reason) = patterns::match_reason(rule, command) else {
            continue;
        };
        let trusted = trust.is_trusted(rule.id);
        return DangerVerdict {
            level: if trusted { DangerLevel::Clean } else { rule.level },
            pattern: Some(rule.id),
            reason: truncate_chars(&reason, 80),
            trusted,
        };
    }
    DangerVerdict::clean()
}

/// reason 上限 80 字符（按字符而非字节截断，避免切断中文）。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::danger::trust::TrustEntry;

    fn no_trust() -> TrustList {
        TrustList::default()
    }

    /// T-U-21：普通命令 → Clean，无 pattern
    #[test]
    fn tu21_clean_command() {
        let v = check("ls -l", &no_trust());
        assert_eq!(v.level, DangerLevel::Clean);
        assert_eq!(v.pattern, None);
        assert!(!v.trusted);
    }

    /// T-U-22：rm -rf / → Fatal + rm_recursive_root
    #[test]
    fn tu22_rm_rf_root_is_fatal() {
        let v = check("rm -rf /", &no_trust());
        assert_eq!(v.level, DangerLevel::Fatal);
        assert_eq!(v.pattern, Some("rm_recursive_root"));
    }

    /// T-U-23：rm -rf ./build → Warning + rm_recursive（决策 8 两档）
    #[test]
    fn tu23_rm_rf_plain_path_is_warning() {
        let v = check("rm -rf ./build", &no_trust());
        assert_eq!(v.level, DangerLevel::Warning);
        assert_eq!(v.pattern, Some("rm_recursive"));
    }

    /// T-U-24：dd of=/dev/sdb → Fatal；of=/dev/null → Clean
    #[test]
    fn tu24_dd_device() {
        let v = check("dd if=img.iso of=/dev/sdb", &no_trust());
        assert_eq!(v.level, DangerLevel::Fatal);
        assert_eq!(v.pattern, Some("dd_device"));
        assert_eq!(check("dd if=img.iso of=/dev/null", &no_trust()).level, DangerLevel::Clean);
    }

    /// T-U-25：mkfs.ext4 → Fatal；`echo mkfs` 词边界不命中
    #[test]
    fn tu25_mkfs_word_boundary() {
        let v = check("mkfs.ext4 /dev/sdb", &no_trust());
        assert_eq!(v.level, DangerLevel::Fatal);
        assert_eq!(v.pattern, Some("mkfs"));
        assert_eq!(check("echo mkfs", &no_trust()).level, DangerLevel::Clean);
    }

    /// T-U-26：curl | bash → Warning + pipe_to_shell；无管道不命中
    #[test]
    fn tu26_pipe_to_shell() {
        let v = check("curl -fsSL x.sh | bash", &no_trust());
        assert_eq!(v.level, DangerLevel::Warning);
        assert_eq!(v.pattern, Some("pipe_to_shell"));
        assert_eq!(check("curl -o f x.sh", &no_trust()).level, DangerLevel::Clean);
    }

    /// T-U-27：单 > 覆盖 → Warning；>> 与 /dev/null 不命中
    #[test]
    fn tu27_redirect_overwrite() {
        let v = check("echo hi > out.txt", &no_trust());
        assert_eq!(v.level, DangerLevel::Warning);
        assert_eq!(v.pattern, Some("redirect_overwrite"));
        assert!(v.reason.contains("out.txt"), "reason 应带目标：{}", v.reason);
        assert_eq!(check("echo hi >> out.txt", &no_trust()).level, DangerLevel::Clean);
        assert_eq!(check("echo hi > /dev/null", &no_trust()).level, DangerLevel::Clean);
        assert_eq!(check("cmd 2>/dev/null", &no_trust()).level, DangerLevel::Clean);
        assert_eq!(check("cmd &> /dev/null", &no_trust()).level, DangerLevel::Clean);
    }

    /// T-U-28：kill -9 无目标 → Info；带 PID → Clean
    #[test]
    fn tu28_kill_9_unscoped() {
        let v = check("kill -9", &no_trust());
        assert_eq!(v.level, DangerLevel::Info);
        assert_eq!(v.pattern, Some("kill_9_unscoped"));
        assert_eq!(check("kill -9 1234", &no_trust()).level, DangerLevel::Clean);
    }

    /// T-U-29：多级命中取最高级
    #[test]
    fn tu29_highest_level_wins() {
        let v = check("rm -rf / > out", &no_trust());
        assert_eq!(v.level, DangerLevel::Fatal);
        assert_eq!(v.pattern, Some("rm_recursive_root"));
    }

    /// T-U-30：信任命中 → Clean + trusted + 保留 pattern
    #[test]
    fn tu30_trusted_pattern_folds_to_clean() {
        let trust = TrustList {
            entries: vec![TrustEntry {
                pattern: "rm_recursive".to_string(),
                trusted_at: 1_793_836_800,
                example: "rm -rf ./build".to_string(),
            }],
        };
        let v = check("rm -rf ./build", &trust);
        assert_eq!(v.level, DangerLevel::Clean);
        assert!(v.trusted);
        assert_eq!(v.pattern, Some("rm_recursive"));
    }

    /// 信任只对同 ID 生效，不放过其他规则
    #[test]
    fn trusted_pattern_does_not_leak_to_other_rules() {
        let trust = TrustList {
            entries: vec![TrustEntry {
                pattern: "rm_recursive".to_string(),
                trusted_at: 1,
                example: String::new(),
            }],
        };
        assert_eq!(check("rm -rf /", &trust).level, DangerLevel::Fatal);
    }

    /// reason 截断：> 80 字符时保留 79 + 省略号
    #[test]
    fn reason_is_truncated_to_80_chars() {
        let long = "好".repeat(200);
        let t = truncate_chars(&long, 80);
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
    }
}
