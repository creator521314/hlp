use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::HlpError;

/// 顶层配置，对应 `~/.config/hlp/config.toml`。
/// schema 与默认值见 docs/design/v0.1-engine.md §3.1。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Provider 名，v0.1 仅支持 "ollama"
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub ollama: OllamaConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            ollama: OllamaConfig::default(),
        }
    }
}

fn default_provider() -> String {
    "ollama".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
    pub max_candidates: usize,
    pub keep_alive: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".to_string(),
            model: "qwen2.5-coder:3b".to_string(),
            timeout_ms: 60_000,
            max_candidates: 3,
            keep_alive: "5m".to_string(),
        }
    }
}

impl Config {
    /// 校验规则见设计文档 §2.2。一切配置错误均为直接退出类（退出码 3），
    /// 不允许带病运行。
    fn validate(&self, path: &Path) -> Result<(), HlpError> {
        let invalid = |reason: String| HlpError::ConfigInvalid {
            path: path.to_path_buf(),
            reason,
        };
        if self.provider != "ollama" {
            return Err(invalid(format!(
                "provider 尚未实现：{}（v0.1 仅支持 ollama）",
                self.provider
            )));
        }
        let o = &self.ollama;
        if o.base_url.trim().is_empty() {
            return Err(invalid("ollama.base_url 不能为空".into()));
        }
        if o.model.trim().is_empty() {
            return Err(invalid("ollama.model 不能为空".into()));
        }
        if !(1_000..=600_000).contains(&o.timeout_ms) {
            return Err(invalid(format!(
                "ollama.timeout_ms 须在 1000..=600000，当前值：{}",
                o.timeout_ms
            )));
        }
        if !(1..=10).contains(&o.max_candidates) {
            return Err(invalid(format!(
                "ollama.max_candidates 须在 1..=10，当前值：{}",
                o.max_candidates
            )));
        }
        if o.keep_alive.trim().is_empty() {
            return Err(invalid("ollama.keep_alive 不能为空".into()));
        }
        Ok(())
    }
}

/// 配置文件路径：`$XDG_CONFIG_HOME/hlp/config.toml`（Linux 下即
/// `~/.config/hlp/config.toml`）。v0.1 只读不写。
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hlp")
        .join("config.toml")
}

/// 加载配置。文件不存在 → 全默认值，不视为错误，也不创建目录。
pub fn load() -> Result<Config, HlpError> {
    load_at(&config_path())
}

/// 从指定路径加载（供单元测试注入路径）。
pub(crate) fn load_at(path: &Path) -> Result<Config, HlpError> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(path).map_err(|source| HlpError::ConfigIo {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: Config = toml::from_str(&text).map_err(|e| HlpError::ConfigInvalid {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    cfg.validate(path)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hlp-test-{}-{}.toml", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn write(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    /// T-U-01：配置文件不存在 → 全默认值
    #[test]
    fn tu01_missing_file_returns_defaults() {
        let cfg = load_at(&tmp_path("missing")).unwrap();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.ollama.model, "qwen2.5-coder:3b");
        assert_eq!(cfg.ollama.timeout_ms, 60_000);
        assert_eq!(cfg.ollama.max_candidates, 3);
        assert_eq!(cfg.ollama.keep_alive, "5m");
    }

    /// T-U-02：默认 base_url 仅指向 127.0.0.1（呼应 REQUIREMENTS §12：默认纯本地）
    #[test]
    fn tu02_default_base_url_is_local() {
        let cfg = Config::default();
        assert_eq!(cfg.ollama.base_url, "http://127.0.0.1:11434");
    }

    /// T-U-03：非法 TOML → ConfigInvalid，reason 含定位信息
    #[test]
    fn tu03_invalid_toml() {
        let path = tmp_path("bad-toml");
        write(&path, "provider = \n[ollama\n");
        let err = load_at(&path).unwrap_err();
        match err {
            HlpError::ConfigInvalid { reason, .. } => {
                assert!(!reason.is_empty(), "reason 应包含解析错误描述");
            }
            other => panic!("期望 ConfigInvalid，实际 {other:?}"),
        }
    }

    /// T-U-04：timeout_ms 越界 → ConfigInvalid
    #[test]
    fn tu04_timeout_out_of_range() {
        let path = tmp_path("bad-timeout");
        write(&path, "[ollama]\ntimeout_ms = 50\n");
        let err = load_at(&path).unwrap_err();
        match err {
            HlpError::ConfigInvalid { reason, .. } => {
                assert!(reason.contains("timeout_ms"), "reason 应指明字段：{reason}");
            }
            other => panic!("期望 ConfigInvalid，实际 {other:?}"),
        }
    }

    /// 额外校验：未知 provider 拒绝
    #[test]
    fn unknown_provider_rejected() {
        let path = tmp_path("bad-provider");
        write(&path, "provider = \"openai\"\n");
        let err = load_at(&path).unwrap_err();
        match err {
            HlpError::ConfigInvalid { reason, .. } => {
                assert!(reason.contains("openai"));
            }
            other => panic!("期望 ConfigInvalid，实际 {other:?}"),
        }
    }
}
