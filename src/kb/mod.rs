//! tldr 知识库门面（v0.4 设计 §2.3）：第 2 层缓存。
//!
//! 哲学同 v0.3 缓存（决策 3）：kb 是加速器不是硬依赖——查询失败一律返回空，
//! 不阻断生成。语料双语：中文意图同时匹配 pages.zh 与 pages（英文）语料，
//! 英文意图只匹配英文语料，避免中文页描述对英文意图的噪声命中。

pub mod fetch;
pub mod parse;
pub mod score;

use std::collections::HashSet;

use rusqlite::Connection;

use crate::cache::db_path;
use crate::cache::store::open_connection;
use crate::config::Config;
use crate::error::HlpError;

pub use parse::{Example, Page};

#[derive(Debug, Clone, PartialEq)]
pub struct KbHit {
    /// 示例命令（含 `{{占位符}}`，原样）
    pub command: String,
    /// 示例描述（展示用）
    pub description: String,
    /// 来源命令名
    pub page: String,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KbStatus {
    pub pages: u32,
    pub examples: u32,
    pub updated_at: Option<i64>,
}

pub struct Kb {
    /// None = 禁用或打开失败（降级为空实现，不触碰 DB）
    conn: Option<Connection>,
    min_score: f64,
}

impl Kb {
    /// 永远 Ok：enabled=false / DB 故障 → 空实现（生成路径静默降级）。
    pub fn from_config(cfg: &Config) -> Result<Kb, HlpError> {
        if !cfg.kb.enabled {
            return Ok(Kb {
                conn: None,
                min_score: cfg.kb.min_score,
            });
        }
        Ok(Kb {
            conn: open_connection(&db_path()).ok(),
            min_score: cfg.kb.min_score,
        })
    }

    /// 供测试/daemon 注入指定 DB 路径。
    #[cfg(test)]
    pub fn open_at(cfg: &Config, db: &std::path::Path) -> Kb {
        Kb {
            conn: if cfg.kb.enabled {
                open_connection(db).ok()
            } else {
                None
            },
            min_score: cfg.kb.min_score,
        }
    }

    /// 意图 → 达阈值（>= min_score）的命中，按 score 降序，最多 3 条。
    /// 一切错误（DB 未建/语料为空/查询错误）吞为空 vec。
    pub fn lookup(&self, intent: &str) -> Vec<KbHit> {
        let Some(conn) = self.conn.as_ref() else {
            return Vec::new();
        };
        let intent_tokens = score::tokenize(intent);
        if intent_tokens.is_empty() {
            return Vec::new();
        }
        // 意图集合只构建一次，供全部示例复用。
        let intent_set: HashSet<&str> = intent_tokens.iter().map(String::as_str).collect();
        let has_cjk = intent.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        // 评分只用 example_desc / example_cmd / page.command，不读 kb_pages.description，
        // 因此这里不做逐行相关子查询（真实语料 3.5 万行时可省掉数万次索引查找）。
        let sql = if has_cjk {
            // 中文意图：中英语料都参与
            "SELECT e.command, e.example_cmd, e.example_desc FROM kb_examples e"
        } else {
            "SELECT e.command, e.example_cmd, e.example_desc FROM kb_examples e
             WHERE e.lang = 'en'"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        let mut hits: Vec<KbHit> = Vec::new();
        for row in rows.flatten() {
            let (page_name, example_cmd, example_desc) = row;
            let s = score::score_parts(&intent_set, &example_desc, &example_cmd, &page_name);
            if s >= self.min_score {
                hits.push(KbHit {
                    command: example_cmd,
                    description: example_desc,
                    page: page_name,
                    score: s,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(3);
        hits
    }

    pub fn status(&self) -> KbStatus {
        let Some(conn) = self.conn.as_ref() else {
            return KbStatus::default();
        };
        let count = |sql: &str| -> u32 {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(0)
                .max(0) as u32
        };
        let updated_at = conn
            .query_row("SELECT v FROM kb_meta WHERE k='updated_at'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse::<i64>().ok());
        KbStatus {
            pages: count("SELECT count(*) FROM kb_pages"),
            examples: count("SELECT count(*) FROM kb_examples"),
            updated_at,
        }
    }

    /// 按命令名取页（explain 本地标注用）；platform 优先 linux 回退 common，
    /// 语言优先 zh 回退 en。
    pub fn page_of(&self, command: &str) -> Option<Page> {
        let conn = self.conn.as_ref()?;
        let (lang, description): (String, String) = conn
            .query_row(
                "SELECT lang, description FROM kb_pages WHERE command = ?1
                 ORDER BY CASE platform WHEN 'linux' THEN 0 ELSE 1 END,
                          CASE lang WHEN 'zh' THEN 0 ELSE 1 END LIMIT 1",
                [command],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT example_cmd, example_desc FROM kb_examples
                 WHERE command = ?1 AND lang = ?2 ORDER BY id",
            )
            .ok()?;
        let examples: Vec<Example> = stmt
            .query_map(rusqlite::params![command, lang], |r| {
                Ok(Example {
                    command: r.get(0)?,
                    description: r.get(1)?,
                })
            })
            .ok()?
            .flatten()
            .collect();
        Some(Page {
            command: command.to_string(),
            description,
            examples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// T-U-62：enabled=false → lookup 空 vec，不触碰 DB
    #[test]
    fn tu62_disabled_kb_never_touches_db() {
        let mut cfg = Config::default();
        cfg.kb.enabled = false;
        // 用一个必然打不开的路径：若实现仍开 DB，会走进错误分支但也不该 panic
        let kb = Kb::open_at(&cfg, &PathBuf::from("/proc/definitely/not/a/db.sqlite"));
        assert!(kb.lookup("列出大文件").is_empty());
        assert_eq!(kb.status(), KbStatus::default());
        assert!(kb.page_of("find").is_none());
    }

    /// 空库（无语料）→ 静默空结果
    #[test]
    fn empty_db_degrades_to_no_hits() {
        let dir = tempfile::tempdir().unwrap().keep();
        let db = dir.join("kb.sqlite");
        let cfg = Config::default();
        let kb = Kb::open_at(&cfg, &db);
        assert!(kb.lookup("列出大文件").is_empty());
        assert_eq!(kb.status().pages, 0);
    }
}
