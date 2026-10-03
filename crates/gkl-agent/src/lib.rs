//! gkl-agent: AI 考古助手。
//!
//! 两种模式:
//! 1. LLM(OpenAI 兼容 API):把索引摘要作为上下文,回答自然语言考古问题。
//! 2. 离线启发式(无 API key 时):基于关键词路由到本地查询,给出确定性回答。
//!
//! 原则:agent 只读。收集上下文 -> 提问 -> 引用证据(带 gkl:// 深链接)。

pub mod llm;
pub mod offline;

/// 一次问答的完整结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Answer {
    /// 面向人的回答文本。
    pub text: String,
    /// 引用的深链接证据。
    pub evidence: Vec<String>,
    /// 使用了哪种引擎。
    pub engine: Engine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Engine {
    Llm,
    Heuristic,
}

/// 从环境变量读取 API 配置。
pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

impl LlmConfig {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("GKL_OPENAI_API_KEY").ok()?;
        if api_key.is_empty() {
            return None;
        }
        Some(Self {
            api_key,
            base_url: std::env::var("GKL_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model: std::env::var("GKL_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
        })
    }
}
