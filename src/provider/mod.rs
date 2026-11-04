use crate::error::HlpError;

pub mod ollama;

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
/// v0.1 只定义同步 `generate`；v0.3 的流式将以"新增带默认实现的方法"
/// 方式扩展（如 `generate_stream`，默认实现委托 `generate`），保持向后兼容。
pub trait Provider {
    fn name(&self) -> &'static str;
    fn generate(&self, req: &GenerateRequest) -> Result<GenerateResponse, HlpError>;
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
}
