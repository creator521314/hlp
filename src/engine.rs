use std::time::{Duration, Instant};

use crate::cli::CliArgs;
use crate::config::Config;
use crate::error::HlpError;
use crate::prompt;
use crate::provider::ollama::OllamaProvider;
use crate::provider::{GenerateRequest, Provider};

/// 编排引擎：意图 → Provider → top1（契约见 docs/design/v0.1-engine.md §2.6）。
///
/// engine 自身不打印任何字符，输出全部委托 `output.rs`
/// （保证 stdout 纯净协议只有一个守门人）。
pub struct Engine {
    provider: Box<dyn Provider>,
    model: String,
    max_candidates: usize,
}

/// 引擎产出，供 output 层渲染。
pub struct EngineOutcome {
    /// 选中命令（top1 = candidates[0]，Provider 已保证有序、非空）
    pub top1: String,
    /// 其余候选（可能为空）
    pub alternatives: Vec<String>,
    pub provider_name: &'static str,
    pub model: String,
    pub elapsed: Duration,
}

impl Engine {
    pub fn from_config(cfg: Config) -> Result<Self, HlpError> {
        let provider: Box<dyn Provider> = match cfg.provider.as_str() {
            "ollama" => Box::new(OllamaProvider::new(&cfg.ollama)),
            // config::load 已校验 provider，此分支为防御性兜底
            other => {
                return Err(HlpError::ConfigInvalid {
                    path: crate::config::config_path(),
                    reason: format!("provider 尚未实现：{other}"),
                })
            }
        };
        Ok(Self {
            model: cfg.ollama.model,
            max_candidates: cfg.ollama.max_candidates,
            provider,
        })
    }

    /// 供测试注入 mock Provider。
    #[cfg(test)]
    pub(crate) fn with_provider(
        provider: Box<dyn Provider>,
        model: String,
        max_candidates: usize,
    ) -> Self {
        Self {
            provider,
            model,
            max_candidates,
        }
    }

    pub fn run(&self, args: &CliArgs) -> Result<EngineOutcome, HlpError> {
        let start = Instant::now();
        let req = GenerateRequest {
            intent: args.intent.clone(),
            max_candidates: self.max_candidates,
            system_prompt: prompt::build_system_prompt(self.max_candidates),
        };
        let mut candidates = self.provider.generate(&req)?.candidates;
        let top1 = candidates.remove(0).command;
        let alternatives = candidates.into_iter().map(|c| c.command).collect();
        Ok(EngineOutcome {
            top1,
            alternatives,
            provider_name: self.provider.name(),
            model: self.model.clone(),
            elapsed: start.elapsed(),
        })
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
        }
    }

    /// T-U-10：mock Provider 返回 3 候选 → top1 为第 0 候选，alternatives 为后 2 条
    #[test]
    fn tu10_top1_and_alternatives() {
        let engine = Engine::with_provider(
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
    }
}
