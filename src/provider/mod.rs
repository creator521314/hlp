use crate::config::Config;
use crate::error::HlpError;

pub mod ollama;

/// 从配置构造 Provider（engine 与 daemon 共用）。
/// provider 合法性在 config::load 已校验，此处匹配失败属防御性兜底。
pub fn from_config(cfg: &Config) -> Result<Box<dyn Provider>, HlpError> {
    match cfg.provider.as_str() {
        "ollama" => Ok(Box::new(ollama::OllamaProvider::new(&cfg.ollama))),
        other => Err(HlpError::ConfigInvalid {
            path: crate::config::config_path(),
            reason: format!("provider 尚未实现：{other}"),
        }),
    }
}

/// 一次生成请求。Provider 不感知 system_prompt 的内容（由 prompt.rs 构建）。
pub struct GenerateRequest {
    /// 用户原始意图（不做改写）
    pub intent: String,
    /// 期望候选数上限
    pub max_candidates: usize,
    /// 系统提示词
    pub system_prompt: String,
}

/// 一条候选命令。
pub struct Candidate {
    /// 一条完整可执行命令：单行、无 Markdown 围栏、无首尾空白。
    /// 清洗责任在 Provider，engine 拿到的即是可直接上屏的最终形态。
    pub command: String,
}

/// 生成结果。
pub struct GenerateResponse {
    /// 有序候选，第 0 个为 top1。契约保证非空（空结果由 Provider 转为
    /// `HlpError::NoCandidates`）。
    pub candidates: Vec<Candidate>,
}

/// LLM 后端抽象（REQUIREMENTS §6.3：插件化 Provider 接口）。
///
/// v0.3 流式扩展：`generate_stream` 带**默认实现**（退化为同步 generate，
/// 完成后一次性回调 top1），只实现同步 `generate` 的 Provider 自动获得
/// 向后兼容的流式能力。
pub trait Provider {
    fn name(&self) -> &'static str;
    fn generate(&self, req: &GenerateRequest) -> Result<GenerateResponse, HlpError>;

    /// 流式生成：`on_chunk` 逐次收到"当前最优候选快照"（**全量替换**语义，
    /// 非增量 delta——消费方直接整体覆盖 buffer，见 v0.3 设计 §2.1）。
    /// 快照已经过候选清洗；中途断连 → ProviderUnreachable。
    fn generate_stream(
        &self,
        req: &GenerateRequest,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<GenerateResponse, HlpError> {
        let resp = self.generate(req)?;
        if let Some(top) = resp.candidates.first() {
            on_chunk(&top.command);
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-05：请求字段原样保留，intent 未被改写
    #[test]
    fn tu05_request_fields_preserved() {
        let req = GenerateRequest {
            intent: "把当前目录下所有超过100MB的文件列出来".to_string(),
            max_candidates: 3,
            system_prompt: "SYSTEM".to_string(),
        };
        assert_eq!(req.intent, "把当前目录下所有超过100MB的文件列出来");
        assert_eq!(req.system_prompt, "SYSTEM");
        assert_eq!(req.max_candidates, 3);
    }

    /// T-U-50：默认流式实现——只实现 generate 的 Provider，
    /// generate_stream 一次性回调 top1
    #[test]
    fn tu50_default_stream_impl_calls_back_once() {
        struct SyncOnly;
        impl Provider for SyncOnly {
            fn name(&self) -> &'static str {
                "sync-only"
            }
            fn generate(&self, _req: &GenerateRequest) -> Result<GenerateResponse, HlpError> {
                Ok(GenerateResponse {
                    candidates: vec![
                        Candidate { command: "top1".to_string() },
                        Candidate { command: "alt".to_string() },
                    ],
                })
            }
        }
        let req = GenerateRequest {
            intent: "x".to_string(),
            max_candidates: 3,
            system_prompt: String::new(),
        };
        let mut seen: Vec<String> = Vec::new();
        let resp = SyncOnly
            .generate_stream(&req, &mut |snap| seen.push(snap.to_string()))
            .unwrap();
        assert_eq!(resp.candidates.len(), 2);
        assert_eq!(seen, vec!["top1".to_string()]);
    }
}
