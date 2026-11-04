//! 缓存门面（v0.3 设计 §2.7）：lookup / learn / clear + 目录豁免。
//!
//! 核心原则（决策 3）：缓存是加速器不是硬依赖——生成路径上的 lookup / learn
//! 一律吞错降级（None / 静默丢弃）；只有用户显式操作（`--clear`、`cache-learn`）
//! 的失败才会以 CacheUnavailable（退出码 6）浮出。

pub mod matcher;
pub mod store;

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::HlpError;
use matcher::normalize_intent;
use store::Store;

pub use matcher::MatchKind;
pub use store::{Entry, Source};

/// 缓存 DB 路径：`$XDG_DATA_HOME/hlp/hlp.sqlite`（Linux 下 `~/.local/share/hlp/`）。
pub fn db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hlp")
        .join("hlp.sqlite")
}

/// 缓存句柄。`store=None` 表示禁用（enabled=false 或打开失败降级）——
/// 所有操作退化为 no-op，主流程永远不因缓存而失败。
pub struct Cache {
    store: Option<Store>,
    exempt: Vec<PathBuf>,
}

impl Cache {
    /// 严格打开：失败返回 Err（供 `--clear` / `cache-learn` 报退出码 6）。
    pub fn from_config(cfg: &Config) -> Result<Cache, HlpError> {
        Self::open_at(cfg, &db_path())
    }

    /// 从指定 DB 路径打开（供测试注入；生产路径见 `db_path`）。
    pub fn open_at(cfg: &Config, db_path: &Path) -> Result<Cache, HlpError> {
        if !cfg.cache.enabled {
            return Ok(Self::disabled(cfg));
        }
        let store = Store::open(db_path)?;
        Ok(Cache {
            store: Some(store),
            exempt: cfg.cache.exempt_paths(),
        })
    }

    /// 尽力打开：失败降级为禁用（生成路径用，决策 3）。
    pub fn open_best_effort(cfg: &Config) -> Cache {
        Self::open_best_effort_at(cfg, &db_path())
    }

    /// 同上，从指定路径打开（供测试注入）。
    pub fn open_best_effort_at(cfg: &Config, db_path: &Path) -> Cache {
        Self::open_at(cfg, db_path).unwrap_or_else(|_| Self::disabled(cfg))
    }

    fn disabled(cfg: &Config) -> Cache {
        Cache {
            store: None,
            exempt: cfg.cache.exempt_paths(),
        }
    }

    /// 供引擎单元测试注入：完全禁用的实例。
    #[cfg(test)]
    pub(crate) fn disabled_for_test() -> Cache {
        Cache {
            store: None,
            exempt: Vec::new(),
        }
    }

    /// 查询：exact → prefix，命中即 touch（hits+1）。一切错误吞为 None。
    pub fn lookup(&mut self, intent: &str) -> Option<(Entry, MatchKind)> {
        let store = self.store.as_mut()?;
        let norm = normalize_intent(intent);
        if norm.is_empty() {
            return None;
        }
        let hit = store
            .exact(&norm)
            .ok()
            .flatten()
            .map(|e| (e, MatchKind::Exact))
            .or_else(|| {
                store
                    .prefix(&norm)
                    .ok()
                    .flatten()
                    .map(|e| (e, MatchKind::Prefix))
            });
        if hit.is_some() {
            let _ = store.touch(&norm);
        }
        hit
    }

    /// 显式学习（cache-learn / daemon Learn 用）：Store 故障浮出为
    /// CacheUnavailable（退出码 6），由调用方决定静默或上抛。
    /// 豁免目录命中 = 静默成功（设计 §4.5：不记录任何条目）。
    pub fn learn_strict(
        &mut self,
        intent: &str,
        command: &str,
        source: Source,
        cwd: &Path,
    ) -> Result<(), HlpError> {
        if self.is_exempt(cwd) || self.store.is_none() {
            return Ok(());
        }
        self.store
            .as_mut()
            .expect("上面已判 store.is_none()")
            .learn(intent, command, source)
    }

    /// 清空历史。显式操作失败浮出（`hlp --clear` → 退出码 6）。
    pub fn clear(&mut self) -> Result<(), HlpError> {
        match self.store.as_mut() {
            Some(s) => s.clear(),
            None => Ok(()), // 禁用状态下 clear 是 no-op 成功
        }
    }

    /// 目录豁免：词法前缀匹配（`~` 已在配置加载时展开），不 canonicalize（决策 10）。
    pub fn is_exempt(&self, cwd: &Path) -> bool {
        self.exempt.iter().any(|e| cwd.starts_with(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-47：exempt=["~/secrets"] → is_exempt 对展开后的子路径为 true
    #[test]
    fn tu47_exempt_dirs_tilde_expansion() {
        let home = dirs::home_dir().expect("测试环境应有 HOME");
        let mut cfg = Config::default();
        cfg.cache.exempt_dirs = vec!["~/secrets".to_string()];
        let cache = Cache::from_config(&cfg).unwrap();
        assert!(cache.is_exempt(&home.join("secrets").join("a")));
        assert!(!cache.is_exempt(Path::new("/tmp")));
        assert!(!cache.is_exempt(&home.join("other")));
    }

    /// 豁免目录下 learn 静默丢弃，正常目录写入并可命中
    #[test]
    fn exempt_cwd_silently_skips_learn() {
        let mut cfg = Config::default();
        let exempt = std::env::temp_dir().join("hlp-test-exempt-dir");
        cfg.cache.exempt_dirs = vec![exempt.display().to_string()];
        let db = tempfile::tempdir().unwrap().keep().join("hlp").join("t.sqlite");
        let mut cache = Cache::open_at(&cfg, &db).unwrap();

        // 豁免目录下学习 → 不落库（静默成功）
        cache.learn_strict("列出大文件", "find . -size +100M", Source::Adopted, &exempt).unwrap();
        assert!(cache.lookup("列出大文件").is_none());

        // 正常目录下学习 → 命中
        cache.learn_strict("列出大文件", "find . -size +100M", Source::Adopted, Path::new("/tmp")).unwrap();
        let hit = cache.lookup("列出大文件");
        assert_eq!(hit.unwrap().0.command, "find . -size +100M");

        // learn_strict 在豁免目录下静默成功
        cache.learn_strict("x y", "ls", Source::Adopted, &exempt).unwrap();
        assert!(cache.lookup("x y").is_none());
    }

    /// enabled=false → 全部操作 no-op，clear 仍成功
    #[test]
    fn disabled_cache_is_noop() {
        let mut cfg = Config::default();
        cfg.cache.enabled = false;
        let db = tempfile::tempdir().unwrap().keep().join("hlp").join("t.sqlite");
        let mut cache = Cache::open_at(&cfg, &db).unwrap();
        cache.learn_strict("a", "ls", Source::Adopted, Path::new("/tmp")).unwrap();
        assert!(cache.lookup("a").is_none());
        cache.clear().unwrap();
    }

    /// 缓存故障降级：open_best_effort 对坏路径返回禁用实例而非 Err
    #[test]
    fn best_effort_degrades_on_error() {
        // 目录占用导致 sqlite 打开失败
        let bad = tempfile::tempdir().unwrap().keep();
        let cfg = Config::default();
        let mut cache = Cache::open_best_effort_at(&cfg, &bad);
        // 禁用实例：learn/lookup/clear 全部 no-op 且不报错
        cache.learn_strict("a", "ls", Source::Adopted, Path::new("/tmp")).unwrap();
        assert!(cache.lookup("a").is_none());
        cache.clear().unwrap();
    }
}
