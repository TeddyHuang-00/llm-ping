use std::fmt;

use serde::Deserialize;

// ── Shared content event ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ContentEvent {
    Token(String),
    Done(Option<usize>),
    None,
}

// ── SSE line parser ─────────────────────────────────────────────────────────

pub enum SseEvent<'a> {
    Data(&'a str),
    #[allow(dead_code)]
    EventName(&'a str),
    #[allow(dead_code)]
    Comment,
    #[allow(dead_code)]
    Boundary,
}

pub fn next_sse_event(line: &str) -> SseEvent<'_> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() {
        return SseEvent::Boundary;
    }
    #[allow(clippy::option_if_let_else)]
    if let Some(data) = line.strip_prefix("data:") {
        SseEvent::Data(data.trim())
    } else if let Some(name) = line.strip_prefix("event:") {
        SseEvent::EventName(name.trim())
    } else if line.starts_with(':') {
        SseEvent::Comment
    } else {
        SseEvent::Data(line.trim())
    }
}

// ── Provider trait ──────────────────────────────────────────────────────────

pub trait Provider: Sync {
    fn build_body(&self, model: &str, prompt: &str, stream: bool) -> String;
    fn parse_chunk(&self, data: &str) -> ContentEvent;
}

// ── Concrete provider impls ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct OllamaMessage {
    content: Option<String>,
}
#[derive(Deserialize)]
struct OllamaChunk {
    message: Option<OllamaMessage>,
    done: Option<bool>,
    eval_count: Option<usize>,
}
pub struct Ollama;

impl Provider for Ollama {
    fn build_body(&self, model: &str, prompt: &str, stream: bool) -> String {
        serde_json::json!({"model": model, "messages": [{"role": "user", "content": prompt}], "stream": stream}).to_string()
    }
    fn parse_chunk(&self, data: &str) -> ContentEvent {
        if let Ok(chunk) = serde_json::from_str::<OllamaChunk>(data) {
            if let Some(msg) = &chunk.message
                && let Some(content) = &msg.content
                && !content.is_empty()
            {
                return ContentEvent::Token(content.clone());
            }
            if chunk.done.unwrap_or(false) {
                return ContentEvent::Done(chunk.eval_count);
            }
        }
        ContentEvent::None
    }
}

#[derive(Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}
#[derive(Deserialize)]
struct OpenAiChoice {
    delta: OpenAiDelta,
}
#[derive(Deserialize)]
struct OpenAiUsage {
    completion_tokens: Option<usize>,
}
#[derive(Deserialize)]
struct OpenAiChunk {
    choices: Option<Vec<OpenAiChoice>>,
    usage: Option<OpenAiUsage>,
}
pub struct OpenAI;

impl Provider for OpenAI {
    fn build_body(&self, model: &str, prompt: &str, stream: bool) -> String {
        serde_json::json!({
            "model": model, "messages": [{"role": "user", "content": prompt}], "stream": stream,
            "stream_options": if stream { serde_json::json!({"include_usage": true}) } else { serde_json::Value::Null },
        }).to_string()
    }
    fn parse_chunk(&self, data: &str) -> ContentEvent {
        if let Ok(chunk) = serde_json::from_str::<OpenAiChunk>(data) {
            if let Some(usage) = chunk.usage
                && let Some(choices) = &chunk.choices
                && choices.is_empty()
            {
                return ContentEvent::Done(usage.completion_tokens);
            }
            if let Some(choices) = chunk.choices {
                for choice in choices {
                    if let Some(content) = choice.delta.content
                        && !content.is_empty()
                    {
                        return ContentEvent::Token(content);
                    }
                }
            }
        }
        ContentEvent::None
    }
}

#[derive(Deserialize)]
struct AnthropicTextDelta {
    text: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicEvent {
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { delta: AnthropicTextDelta },
    #[serde(rename = "message_delta")]
    MessageDelta { usage: Option<AnthropicUsage> },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct AnthropicUsage {
    output_tokens: Option<usize>,
}
pub struct Anthropic;

impl Provider for Anthropic {
    fn build_body(&self, model: &str, prompt: &str, stream: bool) -> String {
        let mut body = serde_json::json!({"model": model, "max_tokens": 256, "messages": [{"role": "user", "content": prompt}], "stream": stream});
        if !stream && let Some(obj) = body.as_object_mut() {
            obj.remove("stream");
        }
        body.to_string()
    }
    fn parse_chunk(&self, data: &str) -> ContentEvent {
        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
            return match evt {
                AnthropicEvent::ContentBlockDelta { delta } => {
                    if let Some(text) = delta.text
                        && !text.is_empty()
                    {
                        ContentEvent::Token(text)
                    } else {
                        ContentEvent::None
                    }
                }
                AnthropicEvent::MessageDelta { usage } => {
                    ContentEvent::Done(usage.and_then(|u| u.output_tokens))
                }
                AnthropicEvent::Other => ContentEvent::None,
            };
        }
        ContentEvent::None
    }
}

// ── ProviderKind enum ───────────────────────────────────────────────────────

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum ProviderKind {
    Ollama,
    OpenAI,
    Anthropic,
    DeepSeek,
    OpenRouter,
    Gemini,
    Glm,
    Zhipu,
    Zhipuai,
    Zai,
    Kimi,
    Moonshot,
    Moonshotai,
    KimiCn,
    MoonshotaiCn,
    Siliconflow,
    SiliconflowCn,
    Alibaba,
    AlibabaCn,
    Minimax,
    MinimaxCn,
    Groq,
    Together,
    Togetherai,
    Deepinfra,
    FireworksAi,
    Stepfun,
    Xai,
    Perplexity,
    Mistral,
    Cohere,
    Cerebras,
    Nebius,
    NovitaAi,
    Friendli,
    Nvidia,
    Sambanova,
    /// Generic OpenAI-compatible. Requires --url and --model.
    OpenaiCompatible,
    /// Generic Anthropic-compatible. Requires --url and --model.
    AnthropicCompatible,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = format!("{self:?}");
        let kebab = s.chars().fold(String::new(), |mut acc, c| {
            if c.is_uppercase() && !acc.is_empty() {
                acc.push('-');
            }
            acc.push(c.to_ascii_lowercase());
            acc
        });
        f.write_str(&kebab)
    }
}

impl From<&ProviderKind> for Box<dyn Provider> {
    fn from(k: &ProviderKind) -> Self {
        match k {
            ProviderKind::Ollama => Box::new(Ollama),
            ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => Box::new(Anthropic),
            _ => Box::new(OpenAI),
        }
    }
}

impl ProviderKind {
    pub const fn defaults(&self) -> (&str, &str) {
        match self {
            Self::Ollama => ("http://127.0.0.1:11434/api/chat", "gemma4:12b"),
            Self::OpenAI => ("https://api.openai.com/v1/chat/completions", "gpt-4o"),
            Self::Anthropic => (
                "https://api.anthropic.com/v1/messages",
                "claude-sonnet-4-20250514",
            ),
            Self::DeepSeek => (
                "https://api.deepseek.com/v1/chat/completions",
                "deepseek-v4-flash",
            ),
            Self::OpenRouter => ("https://openrouter.ai/api/v1/chat/completions", "auto"),
            Self::Gemini => (
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
                "gemini-2.0-flash",
            ),
            Self::Glm | Self::Zhipu | Self::Zhipuai => (
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "glm-4-plus",
            ),
            Self::Zai => (
                "https://api.z.ai/api/paas/v4/chat/completions",
                "glm-4-plus",
            ),
            Self::Kimi | Self::Moonshot | Self::Moonshotai => (
                "https://api.moonshot.cn/v1/chat/completions",
                "kimi-k2.7-code",
            ),
            Self::KimiCn | Self::MoonshotaiCn => (
                "https://api.moonshot.cn/v1/chat/completions",
                "kimi-k2.7-code-highspeed",
            ),
            Self::Siliconflow => (
                "https://api.siliconflow.cn/v1/chat/completions",
                "moonshotai/Kimi-K2.6",
            ),
            Self::SiliconflowCn => (
                "https://api.siliconflow.cn/v1/chat/completions",
                "baidu/ERNIE-4.5-300B-A47B",
            ),
            Self::Alibaba | Self::AlibabaCn => (
                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
                "qwen3-coder-plus",
            ),
            Self::Minimax | Self::MinimaxCn => (
                "https://api.minimax.chat/v1/chat/completions",
                "MiniMax-M2.1",
            ),
            Self::Groq => (
                "https://api.groq.com/openai/v1/chat/completions",
                "llama-3.3-70b-versatile",
            ),
            Self::Together | Self::Togetherai => (
                "https://api.together.xyz/v1/chat/completions",
                "meta-llama/Llama-3.3-70B-Instruct",
            ),
            Self::Deepinfra => (
                "https://api.deepinfra.com/v1/openai/chat/completions",
                "meta-llama/Llama-3.3-70B-Instruct",
            ),
            Self::FireworksAi => (
                "https://api.fireworks.ai/inference/v1/chat/completions",
                "accounts/fireworks/models/llama-v3p3-70b-instruct",
            ),
            Self::Stepfun => ("https://api.stepfun.ai/v1/chat/completions", "step-1-32k"),
            Self::Xai => ("https://api.x.ai/v1/chat/completions", "grok-4"),
            Self::Perplexity => ("https://api.perplexity.ai/chat/completions", "sonar-pro"),
            Self::Mistral => (
                "https://api.mistral.ai/v1/chat/completions",
                "codestral-latest",
            ),
            Self::Cohere => ("https://api.cohere.ai/v1/chat/completions", "command-a"),
            Self::Cerebras => ("https://api.cerebras.ai/v1/chat/completions", "llama3.1-8b"),
            Self::Nebius => (
                "https://api.studio.nebius.ai/v1/chat/completions",
                "meta-llama/Meta-Llama-3.3-70B-Instruct",
            ),
            Self::NovitaAi => (
                "https://api.novita.ai/v1/chat/completions",
                "meta-llama/llama-3.1-8b-instruct",
            ),
            Self::Friendli => (
                "https://inference.friendli.ai/openai/v1/chat/completions",
                "google/gemma-4-31B-it",
            ),
            Self::Nvidia => (
                "https://integrate.api.nvidia.com/v1/chat/completions",
                "meta/llama-3.3-70b-instruct",
            ),
            Self::Sambanova => (
                "https://api.sambanova.ai/v1/chat/completions",
                "Meta-Llama-3.3-70B-Instruct",
            ),
            Self::OpenaiCompatible | Self::AnthropicCompatible => {
                ("https://localhost/v1/chat/completions", "custom")
            }
        }
    }

    pub const fn api_key_envs(&self) -> &[&str] {
        match self {
            Self::OpenAI => &["OPENAI_API_KEY"],
            Self::Anthropic | Self::AnthropicCompatible => {
                &["ANTHROPIC_API_KEY", "ANTHROPIC_TOKEN"]
            }
            Self::DeepSeek => &["DEEPSEEK_API_KEY"],
            Self::OpenRouter => &["OPENROUTER_API_KEY"],
            Self::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Self::Glm | Self::Zhipu | Self::Zhipuai => &["ZHIPUAI_API_KEY", "GLM_API_KEY"],
            Self::Zai => &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
            Self::Kimi | Self::Moonshot | Self::Moonshotai => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
            Self::KimiCn | Self::MoonshotaiCn => &["KIMI_CN_API_KEY"],
            Self::Siliconflow | Self::SiliconflowCn => &["SILICONFLOW_API_KEY"],
            Self::Alibaba | Self::AlibabaCn => &["ALIBABA_API_KEY", "DASHSCOPE_API_KEY"],
            Self::Minimax => &["MINIMAX_API_KEY"],
            Self::MinimaxCn => &["MINIMAX_CN_API_KEY"],
            Self::Groq => &["GROQ_API_KEY"],
            Self::Together | Self::Togetherai => &["TOGETHER_API_KEY"],
            Self::Deepinfra => &["DEEPINFRA_API_KEY"],
            Self::FireworksAi => &["FIREWORKS_API_KEY"],
            Self::Stepfun => &["STEPFUN_API_KEY"],
            Self::Xai => &["XAI_API_KEY"],
            Self::Perplexity => &["PERPLEXITY_API_KEY"],
            Self::Mistral => &["MISTRAL_API_KEY"],
            Self::Cohere => &["COHERE_API_KEY"],
            Self::Cerebras => &["CEREBRAS_API_KEY"],
            Self::Nebius => &["NEBIUS_API_KEY"],
            Self::NovitaAi => &["NOVITA_API_KEY"],
            Self::Friendli => &["FRIENDLI_API_KEY"],
            Self::Nvidia => &["NVIDIA_API_KEY"],
            Self::Sambanova => &["SAMBA_API_KEY", "SAMBANOVA_API_KEY"],
            Self::OpenaiCompatible => &["CUSTOM_API_KEY"],
            Self::Ollama => &[],
        }
    }
}
