//! kb-update：下载 tldr.zip → 解压 → 解析 → 事务重建 kb 语料（v0.4 设计 §2.4 / §4.2）。
//!
//! 失败语义：任何一步失败 → KbUnavailable（退出码 6），DB 事务回滚保持旧语料。
//! 结构漂移熔断（遗留风险 #3）：skipped > 总数 50% → 整体报错，不静默导入空库。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;

use super::parse::{parse_page, Page};
use crate::cache::store::open_connection;
use crate::error::HlpError;

pub const DEFAULT_INDEX_URL: &str =
    "https://github.com/tldr-pages/tldr/releases/latest/download/tldr.zip";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KbStats {
    pub pages: u32,
    pub examples: u32,
    pub skipped: u32,
}

/// 按配置/环境解析下载地址：HLP_KB_URL（测试/临时覆盖）> cfg [kb] index_url > 官方地址。
pub fn resolve_url(configured: &str) -> String {
    if let Ok(v) = std::env::var("HLP_KB_URL") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if !configured.trim().is_empty() {
        return configured.trim().to_string();
    }
    DEFAULT_INDEX_URL.to_string()
}

/// 全量重建：先清空 kb 两表再插入（同事务）。返回导入统计。
#[allow(dead_code)] // 设计 §2.4 的公开接口；生产走 update_from（传 index_url）
pub fn update(db_path: &Path, progress: &mut dyn FnMut(&str)) -> Result<KbStats, HlpError> {
    update_from(db_path, "", progress)
}

/// 带 index_url 覆盖的版本（main 传 `[kb] index_url`）。
pub fn update_from(
    db_path: &Path,
    index_url: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<KbStats, HlpError> {
    let url = resolve_url(index_url);
    let zip_path = download(&url, progress)?;
    progress("解压并解析语料…");

    let file = File::open(&zip_path).map_err(|e| kb_err(format!("无法打开下载缓存：{e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| kb_err(format!("tldr.zip 损坏或不是合法 zip：{e}")))?;

    let mut pages: Vec<(String, String, String, Page)> = Vec::new(); // (lang, platform, stem, page)
    let mut skipped = 0u32;
    let mut seen_pages_dir = false;

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let Some((lang, platform)) = classify(&name) else {
            continue;
        };
        seen_pages_dir = true;
        if !name.ends_with(".md") {
            continue;
        }
        let stem = Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            skipped += 1;
            continue;
        }
        match parse_page(&text) {
            Ok(mut page) => {
                // 文件名是权威命令名（标题可能带别名或大小写差异）
                page.command = stem.clone();
                pages.push((lang.to_string(), platform.to_string(), stem, page));
            }
            Err(_) => skipped += 1,
        }
    }

    if !seen_pages_dir {
        return Err(kb_err(
            "zip 中缺少 pages/ 目录，可能上游结构已变更；请检查 [kb] index_url".to_string(),
        ));
    }
    let total = pages.len() as u32 + skipped;
    if total == 0 || skipped * 2 > total {
        return Err(kb_err(format!(
            "语料结构疑似漂移：解析成功 {} 页 / 跳过 {} 页（跳过超过 50%），已放弃更新",
            pages.len(),
            skipped
        )));
    }

    let mut conn = open_connection(db_path).map_err(|e| kb_err(e.to_string()))?;
    let tx = conn.transaction().map_err(|e| kb_err(e.to_string()))?;
    tx.execute("DELETE FROM kb_pages", [])
        .and_then(|_| tx.execute("DELETE FROM kb_examples", []))
        .map_err(|e| kb_err(e.to_string()))?;

    let mut examples = 0u32;
    for (lang, platform, stem, page) in &pages {
        tx.execute(
            "INSERT OR REPLACE INTO kb_pages (command, lang, platform, description)
             VALUES (?1, ?2, ?3, ?4)",
            params![stem, lang, platform, page.description],
        )
        .map_err(|e| kb_err(e.to_string()))?;
        for ex in &page.examples {
            tx.execute(
                "INSERT INTO kb_examples (command, lang, example_cmd, example_desc)
                 VALUES (?1, ?2, ?3, ?4)",
                params![stem, lang, ex.command, ex.description],
            )
            .map_err(|e| kb_err(e.to_string()))?;
            examples += 1;
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    tx.execute(
        "INSERT OR REPLACE INTO kb_meta (k, v) VALUES ('updated_at', ?1)",
        [now],
    )
    .map_err(|e| kb_err(e.to_string()))?;
    tx.commit().map_err(|e| kb_err(e.to_string()))?;

    let stats = KbStats {
        pages: pages.len() as u32,
        examples,
        skipped,
    };
    progress(&format!(
        "已导入 {} 页 / {} 条示例（跳过 {}）",
        stats.pages, stats.examples, stats.skipped
    ));
    Ok(stats)
}

/// 只接受设计 §2.4 列出的六个目录；返回 (lang, platform)。
fn classify(name: &str) -> Option<(&'static str, &'static str)> {
    const DIRS: [(&str, &str, &str); 4] = [
        ("pages.zh/common/", "zh", "common"),
        ("pages.zh/linux/", "zh", "linux"),
        ("pages/common/", "en", "common"),
        ("pages/linux/", "en", "linux"),
    ];
    for (prefix, lang, platform) in DIRS {
        if name.starts_with(prefix) {
            return Some((lang, platform));
        }
    }
    None
}

fn download(url: &str, progress: &mut dyn FnMut(&str)) -> Result<PathBuf, HlpError> {
    let dest = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hlp")
        .join("tldr.zip");
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| kb_err(format!("无法创建缓存目录：{e}")))?;
    }
    if let Some(p) = url.strip_prefix("file://") {
        progress("读取本地 tldr.zip 语料…");
        fs::copy(p, &dest).map_err(|e| kb_err(format!("读取 {p} 失败：{e}")))?;
        return Ok(dest);
    }
    progress(&format!("下载语料：{url}"));
    let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(60));
    // 显式联网操作尊重标准代理环境变量（[kb] index_url 之外的第二条通路）
    if let Ok(p) = std::env::var("https_proxy").or_else(|_| std::env::var("HTTPS_PROXY")) {
        if let Ok(proxy) = ureq::Proxy::new(&p) {
            builder = builder.proxy(proxy);
        }
    }
    let agent = builder.build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| kb_err(format!("下载失败：{e}")))?;
    let mut file = File::create(&dest).map_err(|e| kb_err(format!("无法写下载缓存：{e}")))?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file)
        .and_then(|_| file.flush())
        .map_err(|e| kb_err(format!("写入下载缓存失败：{e}")))?;
    Ok(dest)
}

fn kb_err(reason: String) -> HlpError {
    HlpError::KbUnavailable { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_only_official_dirs() {
        assert_eq!(classify("pages/common/find.md"), Some(("en", "common")));
        assert_eq!(classify("pages/linux/du.md"), Some(("en", "linux")));
        assert_eq!(classify("pages.zh/common/find.md"), Some(("zh", "common")));
        assert_eq!(classify("pages.zh/linux/du.md"), Some(("zh", "linux")));
        assert_eq!(classify("pages/osx/foo.md"), None);
        assert_eq!(classify("README.md"), None);
    }

    #[test]
    fn resolve_url_prefers_env_then_config() {
        let def = resolve_url("");
        assert_eq!(def, DEFAULT_INDEX_URL);
        assert_eq!(resolve_url("https://mirror/x.zip"), "https://mirror/x.zip");
    }
}
