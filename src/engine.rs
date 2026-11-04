use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::cache::{Cache, MatchKind};
use crate::cli::CliArgs;
use crate::config::Config;
use crate::daemon::{self, client};
use crate::daemon::protocol::Source;
use crate::error::HlpError;
use crate::prompt;
use crate::provider::GenerateRequest;
use crate::provider::Provider;

/// 编排引擎（v0.3 §2.10）：
///
/// ```text
/// 意图归一化 → 缓存 lookup
///   ├─ 命中 → 单候选返回（source=cache，verbose 标注）
///   └─ 未命中 → daemon-first：
///         ensure_running 成功 → daemon::client::generate（stream 时接快照回调）
///         Transient 失败（拉起失败/协议错误/断连）→ 回退 v0.1 直连 Provider
/// ```
///
/// stdout 纯净协议与退出码集保持不变；缓存故障静默降级（决策 3）、
/// daemon 故障回退直连（决策 5）——两套基础设施都不在正确性关键路径上。
pub struct Engine {
    cfg: Config,
    provider: Box<dyn Provider>,
    model: String,
    max_candidates: usize,
    /// 缓存；打开失败时为禁用实例（no-op）
    cache: Cache,
    /// 单元测试用 with_provider 构造时关闭 daemon 路径，避免真拉起
    use_daemon: bool,
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
}

impl Engine {
    pub fn from_config(cfg: Config) -> Result<Self, HlpError> {
        let provider = crate::provider::from_config(&cfg)?;
        let model = cfg.ollama.model.clone();
        let max_candidates = cfg.ollama.max_candidates;
        // 尽力打开：失败降级为禁用（决策 3）
        let cache = Cache::open_best_effort(&cfg);
        Ok(Engine {
            cfg,
            provider,
            model,
            max_candidates,
            cache,
            use_daemon: true,
        })
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
        Engine {
            cfg,
            provider,
            model,
            max_candidates,
            cache,
            use_daemon: false,
        }
    }

    /// v0.1 主形态（行为契约不变）。
    pub fn run(&mut self, args: &CliArgs) -> Result<EngineOutcome, HlpError> {
        self.run_inner(&args.intent, false, None)
    }

    /// v0.3 流式形态：`on_chunk` 逐快照触发（全量替换语义）。
    pub fn run_stream(
        &mut self,
        intent: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<EngineOutcome, HlpError> {
        self.run_inner(intent, true, Some(on_chunk))
    }

    fn run_inner(
        &mut self,
        intent: &str,
        stream: bool,
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
            });
        }

        let max = self.max_candidates;
        let req = GenerateRequest {
            intent: intent.to_string(),
            max_candidates: max,
            system_prompt: prompt::build_system_prompt(max),
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
