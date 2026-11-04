use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::cache::{Cache, MatchKind};
use crate::cli::CliArgs;
use crate::config::Config;
use crate::daemon::{self, client};
use crate::daemon::protocol::Source;
use crate::error::HlpError;
use crate::explain::Segment;
use crate::kb::Kb;
use crate::prompt;
use crate::provider::GenerateRequest;
use crate::provider::Provider;

/// 编排引擎（v0.4 §4.1 三层链）：
///
/// ```text
/// 意图归一化 → 第 1 层 cache.lookup（v0.3，命中 source=cache）
///   └─ 未命中 → 第 2 层 kb.lookup（v0.4，达阈值 source=kb，占位符原样）
///         └─ 未命中 → 第 3 层 daemon-first LLM（v0.3 不变）
/// ```
///
/// stdout 纯净协议与退出码集保持不变；缓存/kb 故障静默降级（决策 3）、
/// daemon 故障回退直连（决策 5）——基础设施都不在正确性关键路径上。
pub struct Engine {
    cfg: Config,
    provider: Box<dyn Provider>,
    model: String,
    max_candidates: usize,
    /// 缓存；打开失败时为禁用实例（no-op）
    cache: Cache,
    /// tldr 知识库（v0.4 第 2 层）；失败为空实现
    kb: Kb,
    /// 单元测试用 with_provider 构造时关闭 daemon 路径，避免真拉起
    use_daemon: bool,
    /// TUI 卡片请求逐段解释（决策 8）
    want_explanation: bool,
}

/// 引擎产出，供 output 层渲染。
pub struct EngineOutcome {
    /// 选中命令（top1 = candidates[0]，Provider 已保证有序、非空）
    pub top1: String,
    /// 其余候选（可能为空；缓存命中恒为空）
    pub alternatives: Vec<String>,
    pub provider_name: &'static str,
    pub model: String,
    pub elapsed: Duration,
    /// 生成来源（v0.3）：cache | llm
    pub source: Source,
    /// 缓存命中方式（verbose 元信息用；LLM 路径为 None）
    pub match_kind: Option<MatchKind>,
    /// kb 命中分数（verbose 元信息用；其它路径为 None）
    pub kb_score: Option<f64>,
    /// 逐段解释（TUI 卡片；want_explanation=false 时为空）
    pub segments: Vec<Segment>,
}

impl Engine {
    pub fn from_config(cfg: Config) -> Result<Self, HlpError> {
        let provider = crate::provider::from_config(&cfg)?;
        let model = cfg.ollama.model.clone();
        let max_candidates = cfg.ollama.max_candidates;
        // 尽力打开：失败降级为禁用（决策 3）
        let cache = Cache::open_best_effort(&cfg);
        // kb 层：DB 故障 / disabled 都退化为空实现，不阻断生成
        let kb = Kb::from_config(&cfg)?;
        Ok(Engine {
            cfg,
            provider,
            model,
            max_candidates,
            cache,
            kb,
            use_daemon: true,
            want_explanation: false,
        })
    }

    /// TUI 卡片打开逐段解释（决策 8：生成与解释同一次 LLM 调用）。
    pub fn set_want_explanation(&mut self, v: bool) {
        self.want_explanation = v;
    }

    /// 供测试注入 mock Provider（daemon 路径关闭）。
    #[cfg(test)]
    pub(crate) fn with_provider(
        provider: Box<dyn Provider>,
        model: String,
        max_candidates: usize,
    ) -> Self {
        let cfg = Config::default();
        let cache = Cache::disabled_for_test();
        let kb = Kb::open_at(&cfg, &std::path::PathBuf::from("/proc/hlp-test-no-db.sqlite"));
        Engine {
            cfg,
            provider,
            model,
            max_candidates,
            cache,
            kb,
            use_daemon: false,
            want_explanation: false,
        }
    }

    /// v0.1 主形态（行为契约不变）。
    pub fn run(&mut self, args: &CliArgs) -> Result<EngineOutcome, HlpError> {
        self.run_inner(&args.intent, false, args.alt, None)
    }

    /// v0.3 流式形态：`on_chunk` 逐快照触发（全量替换语义）。
    pub fn run_stream(
        &mut self,
        intent: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<EngineOutcome, HlpError> {
        self.run_inner(intent, true, false, Some(on_chunk))
    }

    fn run_inner(
        &mut self,
        intent: &str,
        stream: bool,
        alt: bool,
        mut on_chunk: Option<&mut dyn FnMut(&str)>,
    ) -> Result<EngineOutcome, HlpError> {
        let start = Instant::now();

        // 1. 缓存 lookup（本地，亚秒预算的关键路径）
        if let Some((entry, kind)) = self.cache.lookup(intent) {
            return Ok(EngineOutcome {
                top1: entry.command,
                alternatives: Vec::new(),
                provider_name: self.provider.name(),
                model: self.model.clone(),
                elapsed: start.elapsed(),
                source: Source::Cache,
                match_kind: Some(kind),
                kb_score: None,
                segments: Vec::new(),
            });
        }

        // 2. kb 层（v0.4 决策 3：达 min_score 即整体优先，不与 LLM 混排打分）。
        //    stdout 契约不变：top1 单行、{{占位符}} 原样；--alt 时把 kb 组全部命中作为候选。
        let hits = self.kb.lookup(intent);
        if let Some(top) = hits.first() {
            return Ok(EngineOutcome {
                top1: top.command.clone(),
                alternatives: if alt {
                    hits.iter().skip(1).map(|h| h.command.clone()).collect()
                } else {
                    Vec::new()
                },
                provider_name: self.provider.name(),
                model: self.model.clone(),
                elapsed: start.elapsed(),
                source: Source::Kb,
                match_kind: None,
                kb_score: Some(top.score),
                segments: Vec::new(),
            });
        }

        let max = self.max_candidates;
        let req = GenerateRequest {
            intent: intent.to_string(),
            max_candidates: max,
            system_prompt: prompt::build_system_prompt_with(max, self.want_explanation),
            want_explanation: self.want_explanation,
        };
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // 2. daemon-first（§4.2）：热路径 LLM 感知 < 500ms 的前提
        if self.use_daemon && daemon::ensure_running(&self.cfg).is_ok() {
            let mut forward = |text: &str| {
                if let Some(cb) = on_chunk.as_deref_mut() {
                    cb(text);
                }
            };
            match client::generate(&self.cfg, &req, &cwd, stream, &mut forward) {
                Ok((resp, source)) => {
                    return Ok(self.outcome_from(resp, source, start.elapsed()));
                }
                // daemon 已给出最终结论（provider 错误）：直接上抛
                Err(client::Failure::Permanent(e)) => return Err(e),
                // 基础设施问题：回退 v0.1 直连（决策 5）
                Err(client::Failure::Transient(_)) => {}
            }
        }

        // 3. 直连 Provider（v0.1 路径兜底）；on_chunk 此后不再使用，直接移出
        let resp = match on_chunk {
            Some(cb) => self.provider.generate_stream(&req, cb)?,
            None => self.provider.generate(&req)?,
        };
        Ok(self.outcome_from(resp, Source::Llm, start.elapsed()))
    }

    fn outcome_from(
        &self,
        mut resp: crate::provider::GenerateResponse,
        source: Source,
        elapsed: Duration,
    ) -> EngineOutcome {
        let top1 = resp.candidates.remove(0).command;
        EngineOutcome {
            top1,
            alternatives: resp.candidates.into_iter().map(|c| c.command).collect(),
            provider_name: self.provider.name(),
            model: self.model.clone(),
            elapsed,
            source,
            match_kind: None,
            kb_score: None,
            segments: resp.segments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Candidate, GenerateResponse};

    struct MockProvider {
        commands: Vec<String>,
    }

    impl Provider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn generate(&self, _req: &GenerateRequest) -> Result<GenerateResponse, HlpError> {
            Ok(GenerateResponse {
                candidates: self
                    .commands
                    .iter()
                    .map(|c| Candidate { command: c.clone() })
                    .collect(),
                segments: Vec::new(),
            })
        }
    }

    fn args() -> CliArgs {
        CliArgs {
            intent: "列出大文件".to_string(),
            one_line: true,
            alt: false,
            verbose: false,
            stream: false,
        }
    }

    /// T-U-10：mock Provider 返回 3 候选 → top1 为第 0 候选，alternatives 为后 2 条
    #[test]
    fn tu10_top1_and_alternatives() {
        let mut engine = Engine::with_provider(
            Box::new(MockProvider {
                commands: vec![
                    "find . -type f -size +100M".to_string(),
                    "du -ah . | sort -rh".to_string(),
                    "ls -lS".to_string(),
                ],
            }),
            "mock-model".to_string(),
            3,
        );
        let out = engine.run(&args()).unwrap();
        assert_eq!(out.top1, "find . -type f -size +100M");
        assert_eq!(out.alternatives.len(), 2);
        assert_eq!(out.alternatives[0], "du -ah . | sort -rh");
        assert_eq!(out.alternatives[1], "ls -lS");
        assert_eq!(out.provider_name, "mock");
        assert_eq!(out.source, Source::Llm);
    }

    /// run_stream：直连路径的流式形态回调 top1 快照
    #[test]
    fn run_stream_falls_back_to_provider_stream() {
        let mut engine = Engine::with_provider(
            Box::new(MockProvider {
                commands: vec!["ls -l".to_string()],
            }),
            "mock-model".to_string(),
            3,
        );
        let mut seen = Vec::new();
        let out = engine
            .run_stream("列出文件", &mut |s| seen.push(s.to_string()))
            .unwrap();
        assert_eq!(out.top1, "ls -l");
        assert_eq!(seen, vec!["ls -l".to_string()]);
    }
}
