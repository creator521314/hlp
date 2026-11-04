//! SQLite 存取（v0.3 设计 §2.5）。rusqlite bundled（决策 9），WAL + busy_timeout
//! 5s（遗留风险 #4：daemon 与 cache-learn 直连 DB 可能同时写）。
//!
//! DB 路径 `~/.local/share/hlp/hlp.sqlite`，权限 0600（§3.4）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::cache::matcher::normalize_intent;
use crate::error::HlpError;

/// 学习来源（§4.5：采纳 / 人工修正）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Adopted,
    Correction,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Adopted => "adopted",
            Source::Correction => "correction",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "adopted" => Some(Source::Adopted),
            "correction" => Some(Source::Correction),
            _ => None,
        }
    }
}

/// 前缀匹配的已存意图最小字符数（设计 §2.6：长度 ≥ 4 字符才参与）。
pub const PREFIX_MIN_CHARS: usize = 4;

/// 一条缓存记录的展示形态（不含归一化键）。
/// `intent_raw` / `source` / `hits` 供人工审查与测试断言（生产路径只读 command）。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Entry {
    pub intent_raw: String,
    pub command: String,
    pub source: Source,
    pub hits: u32,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

/// v0.3 schema（user_version = 1）
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS entries (
  id            INTEGER PRIMARY KEY,
  intent_norm   TEXT NOT NULL,
  intent_raw    TEXT NOT NULL,
  command       TEXT NOT NULL,
  source        TEXT NOT NULL,
  hits          INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_intent_norm ON entries(intent_norm);
";

/// v0.4 kb 语料表（设计 §3.2）；与历史缓存同库，`--clear` 只清 entries，
/// 不动公共语料（决策 2）。user_version < 2 时执行，末尾置 2。
const SCHEMA_KB: &str = "
CREATE TABLE IF NOT EXISTS kb_pages (
  command     TEXT NOT NULL,
  lang        TEXT NOT NULL,
  platform    TEXT NOT NULL,
  description TEXT NOT NULL,
  PRIMARY KEY (command, lang, platform)
);
CREATE TABLE IF NOT EXISTS kb_examples (
  id          INTEGER PRIMARY KEY,
  command     TEXT NOT NULL,
  lang        TEXT NOT NULL,
  example_cmd TEXT NOT NULL,
  example_desc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kb_examples_cmd ON kb_examples(command, lang);
CREATE TABLE IF NOT EXISTS kb_meta (k TEXT PRIMARY KEY, v TEXT);
PRAGMA user_version = 2;
";

/// 打开（并在必要时迁移）同库连接。Store / Kb / kb-update 共用，
/// 保证任何入口拿到连接时 kb_* 表都已存在。
pub fn open_connection(path: &Path) -> Result<Connection, HlpError> {
    let err = |reason: String| HlpError::CacheUnavailable {
        path: path.to_path_buf(),
        reason,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| err(format!("无法创建数据目录 {}：{e}", dir.display())))?;
    }
    let conn = Connection::open(path).map_err(|e| err(e.to_string()))?;
    // WAL + busy_timeout：并发写者（daemon / cache-learn / kb-update）
    conn.pragma_update(None, "journal_mode", "WAL")
        .and_then(|_| conn.busy_timeout(Duration::from_millis(5_000)))
        .map_err(|e| err(e.to_string()))?;
    conn.execute_batch(SCHEMA_V1).map_err(|e| err(e.to_string()))?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| err(e.to_string()))?;
    if version < 2 {
        conn.execute_batch(SCHEMA_KB).map_err(|e| err(e.to_string()))?;
    }
    // 数据只归本用户（§3.4：0600）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(conn)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, HlpError> {
        let conn = open_connection(path)?;
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    fn db_err(&self, e: rusqlite::Error) -> HlpError {
        HlpError::CacheUnavailable {
            path: self.path.clone(),
            reason: e.to_string(),
        }
    }

    fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
        let source_str: String = row.get(2)?;
        Ok(Entry {
            intent_raw: row.get(0)?,
            command: row.get(1)?,
            source: Source::parse(&source_str).unwrap_or(Source::Adopted),
            hits: row.get(3)?,
        })
    }

    /// 精确命中：同一归一化意图取 last_used_at 最新的一条。
    pub fn exact(&self, intent_norm: &str) -> Result<Option<Entry>, HlpError> {
        self.conn
            .query_row(
                "SELECT intent_raw, command, source, hits FROM entries
                 WHERE intent_norm = ?1
                 ORDER BY last_used_at DESC, id DESC LIMIT 1",
                [intent_norm],
                Self::row_to_entry,
            )
            .optional()
            .map_err(|e| self.db_err(e))
    }

    /// 前缀命中：已存意图（≥ 4 字符）是当前意图的真前缀，最长前缀优先。
    /// 用 substr/length 而非 LIKE——免转义 %/_ 通配符。
    pub fn prefix(&self, intent_norm: &str) -> Result<Option<Entry>, HlpError> {
        self.conn
            .query_row(
                "SELECT intent_raw, command, source, hits FROM entries
                 WHERE length(?1) > length(intent_norm)
                   AND length(intent_norm) >= ?2
                   AND substr(?1, 1, length(intent_norm)) = intent_norm
                 ORDER BY length(intent_norm) DESC, last_used_at DESC, id DESC
                 LIMIT 1",
                params![intent_norm, PREFIX_MIN_CHARS as i64],
                Self::row_to_entry,
            )
            .optional()
            .map_err(|e| self.db_err(e))
    }

    /// 学习写入（upsert）：同归一化意图 → 更新 command/source、hits+1、last_used_at；
    /// 不存在 → 插入。
    pub fn learn(
        &mut self,
        intent_raw: &str,
        command: &str,
        source: Source,
    ) -> Result<(), HlpError> {
        let norm = normalize_intent(intent_raw);
        if norm.is_empty() {
            return Ok(());
        }
        let now = now_unix();
        let updated = self
            .conn
            .execute(
                "UPDATE entries SET command = ?1, source = ?2, hits = hits + 1,
                 last_used_at = ?3 WHERE intent_norm = ?4",
                params![command, source.as_str(), now, norm],
            )
            .map_err(|e| self.db_err(e))?;
        if updated == 0 {
            self.conn
                .execute(
                    "INSERT INTO entries (intent_norm, intent_raw, command, source, hits,
                     created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
                    params![norm, intent_raw, command, source.as_str(), now],
                )
                .map_err(|e| self.db_err(e))?;
        }
        Ok(())
    }

    /// 命中时 hits+1（lookup 的伴随动作）。
    pub fn touch(&mut self, intent_norm: &str) -> Result<(), HlpError> {
        self.conn
            .execute(
                "UPDATE entries SET hits = hits + 1, last_used_at = ?1 WHERE intent_norm = ?2",
                params![now_unix(), intent_norm],
            )
            .map_err(|e| self.db_err(e))?;
        Ok(())
    }

    /// 清空历史（保留表结构）。
    pub fn clear(&mut self) -> Result<(), HlpError> {
        self.conn
            .execute("DELETE FROM entries", [])
            .map_err(|e| self.db_err(e))?;
        Ok(())
    }

    /// 仅供测试/诊断：条目总数。
    #[allow(dead_code)]
    pub fn count(&self) -> Result<u64, HlpError> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))
            .map_err(|e| self.db_err(e))?;
        Ok(n as u64)
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(name: &str) -> Store {
        let mut p = std::env::temp_dir();
        p.push(format!("hlp-cache-test-{}-{}.sqlite", std::process::id(), name));
        let _ = fs::remove_file(&p);
        let _ = fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = fs::remove_file(p.with_extension("sqlite-shm"));
        Store::open(&p).unwrap()
    }

    /// T-U-42：空 Store 精确查无 → None
    #[test]
    fn tu42_exact_empty() {
        let s = tmp_store("empty");
        assert!(s.exact("列出大文件").unwrap().is_none());
    }

    /// T-U-43：learn 后 exact 命中；touch 使 hits 自增
    #[test]
    fn tu43_exact_hit_and_touch() {
        let mut s = tmp_store("exact");
        s.learn("列出大文件", "find . -size +100M", Source::Adopted).unwrap();
        let e = s.exact("列出大文件").unwrap().unwrap();
        assert_eq!(e.command, "find . -size +100M");
        assert_eq!(e.source, Source::Adopted);
        assert_eq!(e.hits, 1); // 首次学习即计数 1

        s.touch(&normalize_intent("列出大文件")).unwrap();
        let e = s.exact("列出大文件").unwrap().unwrap();
        assert_eq!(e.hits, 2); // lookup 命中即 touch → 自增
    }

    /// T-U-44：前缀命中；前缀长度 < 4 的已存意图不参与
    #[test]
    fn tu44_prefix_hit_and_min_length() {
        let mut s = tmp_store("prefix");
        s.learn("列出大文件", "find . -size +100M", Source::Adopted).unwrap();
        let hit = s.prefix(&normalize_intent("列出大文件并排序")).unwrap();
        assert_eq!(hit.unwrap().command, "find . -size +100M");

        // 已存意图 "列出" 只有 2 字符 → 不参与前缀匹配
        let mut s2 = tmp_store("short-prefix");
        s2.learn("列出", "ls", Source::Adopted).unwrap();
        assert!(s2.prefix(&normalize_intent("列出所有文件")).unwrap().is_none());
    }

    /// T-U-45：多条前缀 → 最长前缀胜出
    #[test]
    fn tu45_longest_prefix_wins() {
        let mut s = tmp_store("longest");
        s.learn("列出", "ls", Source::Adopted).unwrap();
        s.learn("列出大文件", "du -ah . | sort -rh", Source::Adopted).unwrap();
        let hit = s.prefix(&normalize_intent("列出大文件并按大小排序")).unwrap().unwrap();
        assert_eq!(hit.command, "du -ah . | sort -rh");
    }

    /// T-U-46：learn 同意图两次（不同命令）→ 仅一条记录，command 更新，hits=2
    #[test]
    fn tu46_learn_upsert() {
        let mut s = tmp_store("upsert");
        s.learn("列出大文件", "find . -size +100M", Source::Adopted).unwrap();
        s.learn("列出大文件", "du -ah .", Source::Correction).unwrap();
        assert_eq!(s.count().unwrap(), 1);
        let e = s.exact("列出大文件").unwrap().unwrap();
        assert_eq!(e.command, "du -ah .");
        assert_eq!(e.source, Source::Correction);
        assert_eq!(e.hits, 2);
    }

    /// clear 后条目清零，表结构保留（可继续写入）
    #[test]
    fn clear_then_relearn() {
        let mut s = tmp_store("clear");
        s.learn("a b", "ls", Source::Adopted).unwrap();
        s.clear().unwrap();
        assert_eq!(s.count().unwrap(), 0);
        s.learn("a b", "pwd", Source::Adopted).unwrap();
        assert_eq!(s.count().unwrap(), 1);
    }

    /// 归一化是缓存键：大小写/空白差异命中同一条
    #[test]
    fn normalization_is_the_key() {
        let mut s = tmp_store("norm");
        s.learn("LS  -l", "ls -l", Source::Adopted).unwrap();
        assert!(s.exact(&normalize_intent("ls -l")).unwrap().is_some());
        assert_eq!(s.count().unwrap(), 1);
    }

    /// v0.4：schema 迁移后 user_version = 2，kb_* 表就绪
    #[test]
    fn user_version_is_two_with_kb_tables() {
        let s = tmp_store("version");
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
        let tables: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN
                 ('kb_pages','kb_examples','kb_meta','entries')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 4);
        // --clear 只清历史，不清 kb 语料
        s.conn
            .execute(
                "INSERT INTO kb_pages VALUES ('find','en','common','search')",
                [],
            )
            .unwrap();
        s.conn.execute("DELETE FROM entries", []).unwrap();
        let pages: i64 = s
            .conn
            .query_row("SELECT count(*) FROM kb_pages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pages, 1);
    }
}
