//! `~/.config/hlp/danger.toml` 的加载与写入（设计 §2.3 / §3.4）。
//!
//! 单写者原则：本模块是 danger.toml 的**唯一写者**，shell 集成脚本永远不直接改 TOML，
//! 只调 `hlp --trust`。撤销信任走"手动删条目"路线（设计决策 10，不提供 --untrust）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::danger::patterns;
use crate::error::HlpError;

/// 一条信任记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustEntry {
    /// 内置规则 ID（§3.1 的 8 个之一）
    pub pattern: String,
    /// Unix 秒（设计决策 6：不为一个人工审查字段引入时间库依赖）
    pub trusted_at: u64,
    /// 触发时的样例命令，人工审查用（可为空）
    #[serde(default)]
    pub example: String,
}

/// 信任列表。文件不存在时为空列表（合法状态）。
#[derive(Debug, Clone, Default)]
pub struct TrustList {
    pub entries: Vec<TrustEntry>,
}

impl TrustList {
    pub fn is_trusted(&self, pattern_id: &str) -> bool {
        self.entries.iter().any(|e| e.pattern == pattern_id)
    }
}

/// 文件级 schema：顶层只有 `[[trust]]` 数组，未知字段报错（与 config.toml 策略一致）。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    trust: Vec<TrustEntry>,
}

/// `$XDG_CONFIG_HOME/hlp/danger.toml`（Linux 下即 `~/.config/hlp/danger.toml`）。
pub fn danger_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hlp")
        .join("danger.toml")
}

/// 读取信任表。
/// - 文件不存在 → 空 TrustList，不视为错误，也不创建文件
/// - 语法/字段/ID 非法 → Err（退出码 3，提示手动修复路径）
pub fn load() -> Result<TrustList, HlpError> {
    load_at(&danger_path())
}

/// 追加一条信任记录并原子写回（先写 .tmp 再 rename）。
/// 同 ID 重复调用为幂等：已存在则不动文件，返回 Ok。
pub fn add_trust(pattern_id: &str, example: &str) -> Result<(), HlpError> {
    add_trust_at(&danger_path(), pattern_id, example)
}

pub(crate) fn load_at(path: &Path) -> Result<TrustList, HlpError> {
    if !path.exists() {
        return Ok(TrustList::default());
    }
    let text = fs::read_to_string(path).map_err(|source| HlpError::DangerIo {
        path: path.to_path_buf(),
        action: "读取",
        source,
    })?;
    let file: TrustFile = toml::from_str(&text).map_err(|e| HlpError::DangerInvalid {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    for entry in &file.trust {
        if !patterns::is_known_id(&entry.pattern) {
            return Err(HlpError::DangerInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "未知的危险模式 ID：{}（合法 ID：{}）",
                    entry.pattern,
                    patterns::rule_ids().join(", ")
                ),
            });
        }
        if entry.trusted_at == 0 {
            return Err(HlpError::DangerInvalid {
                path: path.to_path_buf(),
                reason: format!("{} 的 trusted_at 须为正整数", entry.pattern),
            });
        }
    }
    Ok(TrustList {
        entries: file.trust,
    })
}

pub(crate) fn add_trust_at(
    path: &Path,
    pattern_id: &str,
    example: &str,
) -> Result<(), HlpError> {
    if !patterns::is_known_id(pattern_id) {
        return Err(HlpError::UnknownPattern {
            id: pattern_id.to_string(),
        });
    }
    let mut list = load_at(path)?;
    if list.is_trusted(pattern_id) {
        return Ok(());
    }
    list.entries.push(TrustEntry {
        pattern: pattern_id.to_string(),
        trusted_at: now_unix(),
        example: example.to_string(),
    });
    write_atomic(path, &list)
}

/// 原子写：同目录写 `danger.toml.tmp` 再 rename，避免半截文件。
fn write_atomic(path: &Path, list: &TrustList) -> Result<(), HlpError> {
    let io_err = |source: io::Error| HlpError::DangerIo {
        path: path.to_path_buf(),
        action: "写入",
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let body = toml::to_string(&TrustFile {
        trust: list.entries.clone(),
    })
    .map_err(|e| HlpError::DangerInvalid {
        path: path.to_path_buf(),
        reason: format!("序列化失败：{e}"),
    })?;
    let mut text = String::from(HEADER);
    text.push_str(&body);

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(io_err)?;
    fs::rename(&tmp, path).map_err(io_err)?;
    Ok(())
}

const HEADER: &str = "\
# ~/.config/hlp/danger.toml —— 危险命令信任列表
# hlp --trust 是唯一写者；撤销信任 = 删除对应 [[trust]] 条目后保存
";

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    /// T-U-31：is_trusted 对未知 ID 返回 false
    #[test]
    fn tu31_is_trusted_unknown_id() {
        let list = TrustList::default();
        assert!(!list.is_trusted("rm_recursive"));
        assert!(!list.is_trusted("no_such_pattern"));
    }

    /// 文件不存在 → 空列表，且不创建文件（首次 --trust 才创建）
    #[test]
    fn missing_file_is_empty_and_not_created() {
        let dir = tmp_dir();
        let path = dir.join("hlp").join("danger.toml");
        let list = load_at(&path).unwrap();
        assert!(list.entries.is_empty());
        assert!(!path.exists(), "读取不应创建文件");
    }

    /// T-U-32：add_trust 幂等——同 ID 二次调用文件内容不变、返回 Ok
    #[test]
    fn tu32_add_trust_is_idempotent() {
        let path = tmp_dir().join("hlp").join("danger.toml");
        add_trust_at(&path, "rm_recursive", "rm -rf ./build").unwrap();
        let first = fs::read_to_string(&path).unwrap();
        add_trust_at(&path, "rm_recursive", "rm -rf ./other").unwrap();
        let second = fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "重复信任同一模式不得改动文件");

        let list = load_at(&path).unwrap();
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].pattern, "rm_recursive");
        assert_eq!(list.entries[0].example, "rm -rf ./build");
        assert!(list.entries[0].trusted_at > 0);
        assert!(first.contains("[[trust]]"));
        assert!(first.contains("example = \"rm -rf ./build\""));
    }

    /// 多条信任按追加顺序保存，互不覆盖
    #[test]
    fn add_multiple_entries() {
        let path = tmp_dir().join("hlp").join("danger.toml");
        add_trust_at(&path, "rm_recursive", "rm -rf a").unwrap();
        add_trust_at(&path, "redirect_overwrite", "echo x > y").unwrap();
        let list = load_at(&path).unwrap();
        assert_eq!(list.entries.len(), 2);
        assert!(list.is_trusted("rm_recursive"));
        assert!(list.is_trusted("redirect_overwrite"));
    }

    /// 未知 pattern → UnknownPattern（退出码 12）
    #[test]
    fn unknown_pattern_rejected() {
        let path = tmp_dir().join("hlp").join("danger.toml");
        let err = add_trust_at(&path, "no_such_pattern", "").unwrap_err();
        assert!(matches!(err, HlpError::UnknownPattern { .. }));
        assert_eq!(err.exit_code(), 12);
    }

    /// 顶层未知字段 → DangerInvalid（退出码 3）
    #[test]
    fn unknown_top_level_field_rejected() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("danger.toml");
        fs::write(&path, "unknown = 1\n").unwrap();
        let err = load_at(&path).unwrap_err();
        assert!(matches!(err, HlpError::DangerInvalid { .. }));
        assert_eq!(err.exit_code(), 3);
    }

    /// danger.toml 里出现未知规则 ID → DangerInvalid（提示合法 ID）
    #[test]
    fn unknown_rule_id_in_file_rejected() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("danger.toml");
        fs::write(&path, "[[trust]]\npattern = \"nope\"\ntrusted_at = 1\nexample = \"\"\n").unwrap();
        let err = load_at(&path).unwrap_err();
        match err {
            HlpError::DangerInvalid { reason, .. } => {
                assert!(reason.contains("rm_recursive"), "应列出合法 ID：{reason}");
            }
            other => panic!("期望 DangerInvalid，实际 {other:?}"),
        }
    }

    /// trusted_at 为 0 → DangerInvalid
    #[test]
    fn zero_trusted_at_rejected() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("danger.toml");
        fs::write(
            &path,
            "[[trust]]\npattern = \"rm_recursive\"\ntrusted_at = 0\nexample = \"\"\n",
        )
        .unwrap();
        assert!(matches!(
            load_at(&path).unwrap_err(),
            HlpError::DangerInvalid { .. }
        ));
    }

    /// example 可省略（缺省空串）
    #[test]
    fn example_is_optional() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("danger.toml");
        fs::write(&path, "[[trust]]\npattern = \"rm_recursive\"\ntrusted_at = 7\n").unwrap();
        let list = load_at(&path).unwrap();
        assert_eq!(list.entries[0].example, "");
    }
}
