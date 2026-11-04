use std::fmt;
use std::io;
use std::path::PathBuf;

/// hlp 错误枚举。分类与退出码契约见 docs/design/v0.1-engine.md §5。
///
/// 设计约束：错误信息面向终端用户，必须给出下一步动作提示（见 `hint`），
/// 不打印 Rust 调试结构。
#[derive(Debug)]
pub enum HlpError {
    /// 配置文件字段值非法（校验失败 / TOML 语法错误）→ 退出码 3
    ConfigInvalid { path: PathBuf, reason: String },
    /// 配置文件存在但不可读 → 退出码 3
    ConfigIo { path: PathBuf, source: io::Error },
    /// LLM 服务不可达（连接拒绝 / DNS / 超时）→ 退出码 4
    ProviderUnreachable { base_url: String, reason: String },
    /// LLM 服务返回非 2xx → 退出码 4
    ProviderHttp { status: u16, body_excerpt: String },
    /// 服务正常但没能给出任何可用候选 → 退出码 5
    NoCandidates { raw_excerpt: String },
}

impl HlpError {
    pub fn exit_code(&self) -> i32 {
        match self {
            HlpError::ConfigInvalid { .. } | HlpError::ConfigIo { .. } => 3,
            HlpError::ProviderUnreachable { .. } | HlpError::ProviderHttp { .. } => 4,
            HlpError::NoCandidates { .. } => 5,
        }
    }

    /// 下一步动作提示，随错误信息打印到 stderr。
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            HlpError::ProviderUnreachable { .. } => {
                Some("请确认 ollama 已启动：ollama serve")
            }
            HlpError::ProviderHttp { status, .. } if *status == 404 => {
                Some("模型可能未拉取，尝试：ollama pull <模型名>（见 config.toml 的 model 字段）")
            }
            HlpError::NoCandidates { .. } => {
                Some("可换个说法重试，或在 ~/.config/hlp/config.toml 中更换 model")
            }
            _ => None,
        }
    }
}

impl fmt::Display for HlpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HlpError::ConfigInvalid { path, reason } => {
                write!(f, "配置无效 {}：{reason}", path.display())
            }
            HlpError::ConfigIo { path, source } => {
                write!(f, "无法读取配置文件 {}：{source}", path.display())
            }
            HlpError::ProviderUnreachable { base_url, reason } => {
                write!(f, "无法连接 LLM 服务（{base_url}）：{reason}")
            }
            HlpError::ProviderHttp { status, body_excerpt } => {
                write!(f, "LLM 服务返回 HTTP {status}：{body_excerpt}")
            }
            HlpError::NoCandidates { raw_excerpt } => {
                write!(f, "未能生成可用命令（原始响应片段：{raw_excerpt}）")
            }
        }
    }
}

impl std::error::Error for HlpError {}
