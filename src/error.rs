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
    /// danger.toml 语法/字段/规则 ID 非法 → 退出码 3（v0.2）
    DangerInvalid { path: PathBuf, reason: String },
    /// danger.toml 读写失败（权限/磁盘）→ 退出码 3（v0.2）
    DangerIo {
        path: PathBuf,
        /// "读取" / "写入"，决定错误文案
        action: &'static str,
        source: io::Error,
    },
    /// `--trust` 的 pattern-id 不在内置规则表中 → 退出码 12（v0.2）
    UnknownPattern { id: String },
}

impl HlpError {
    pub fn exit_code(&self) -> i32 {
        match self {
            HlpError::ConfigInvalid { .. } | HlpError::ConfigIo { .. } => 3,
            HlpError::ProviderUnreachable { .. } | HlpError::ProviderHttp { .. } => 4,
            HlpError::NoCandidates { .. } => 5,
            // v0.2：danger.toml 的问题与 config.toml 同级——配置类错误一律 3
            HlpError::DangerInvalid { .. } | HlpError::DangerIo { .. } => 3,
            // v0.2 新开的"危险判定"段：10/11 由 --check 的 DangerLevel 决定，
            // 12 留给 --trust 的参数错误
            HlpError::UnknownPattern { .. } => 12,
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
            HlpError::DangerInvalid { .. } => Some("请手动修复该文件后重试（删除它即恢复空信任表）"),
            HlpError::DangerIo { .. } => Some("请检查该文件的权限与磁盘空间"),
            HlpError::UnknownPattern { .. } => Some("--trust 只接受内置危险规则 ID（见上列合法 ID）"),
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
            HlpError::DangerInvalid { path, reason } => {
                write!(f, "danger.toml 无效：{}：{reason}", path.display())
            }
            HlpError::DangerIo {
                path,
                action,
                source,
            } => {
                write!(f, "无法{action} danger.toml {}：{source}", path.display())
            }
            HlpError::UnknownPattern { id } => {
                write!(
                    f,
                    "未知的危险模式 ID：{id}（合法 ID：{}）",
                    crate::danger::patterns::rule_ids().join(", ")
                )
            }
        }
    }
}

impl std::error::Error for HlpError {}
