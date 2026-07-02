use serde::Deserialize;

// ── Shared content event ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ContentEvent {
    Token(String),
    Done(Option<usize>),
    None,
}

// ── SSE line parser (shared by all providers) ──────────────────────────────

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

// ── Ollama ──────────────────────────────────────────────────────────────────

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
        serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": stream,
        })
        .to_string()
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

// ── OpenAI-compatible ───────────────────────────────────────────────────────

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
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": stream,
            "stream_options": if stream {
                serde_json::json!({"include_usage": true})
            } else {
                serde_json::Value::Null
            },
        })
        .to_string()
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

// ── Anthropic ───────────────────────────────────────────────────────────────

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
        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": 256,
            "messages": [{"role": "user", "content": prompt}],
            "stream": stream,
        });
        if !stream {
            body.as_object_mut().unwrap().remove("stream");
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

// ── Factory + defaults ──────────────────────────────────────────────────────

pub fn from_type(t: &str) -> Box<dyn Provider> {
    match t {
        "ollama" => Box::new(Ollama),
        "openai" | "deepseek" | "openrouter" | "gemini" | "google" | "glm" | "zhipu"
        | "zhipuai" | "zai" | "kimi" | "moonshot" | "moonshotai" | "kimi-cn" | "moonshotai-cn"
        | "siliconflow" | "siliconflow-cn" | "alibaba" | "alibaba-cn" | "minimax"
        | "minimax-cn" | "groq" | "together" | "togetherai" | "deepinfra" | "fireworks-ai"
        | "stepfun" | "xai" | "perplexity" | "mistral" | "cohere" | "cerebras" | "nebius"
        | "novita-ai" | "friendli" | "nvidia" | "sambanova" => Box::new(OpenAI),
        "anthropic" => Box::new(Anthropic),
        _ => Box::new(OpenAI),
    }
}

/// Default (URL, model). Canonical names from models.dev community database.
pub fn defaults(t: &str) -> (&str, &str) {
    match t {
        "ollama" => ("http://127.0.0.1:11434/v1/chat/completions", "gemma4:12b"),
        "openai" => ("https://api.openai.com/v1/chat/completions", "gpt-4o"),
        "anthropic" => (
            "https://api.anthropic.com/v1/messages",
            "claude-sonnet-4-20250514",
        ),
        "deepseek" => (
            "https://api.deepseek.com/v1/chat/completions",
            "deepseek-v4-flash",
        ),
        "openrouter" => ("https://openrouter.ai/api/v1/chat/completions", "auto"),
        "gemini" | "google" => (
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            "gemini-2.0-flash",
        ),
        "glm" | "zhipu" | "zhipuai" => (
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            "glm-4-plus",
        ),
        "zai" => (
            "https://api.z.ai/api/paas/v4/chat/completions",
            "glm-4-plus",
        ),
        "kimi" | "moonshot" | "moonshotai" => (
            "https://api.moonshot.cn/v1/chat/completions",
            "kimi-k2.7-code",
        ),
        "kimi-cn" | "moonshotai-cn" => (
            "https://api.moonshot.cn/v1/chat/completions",
            "kimi-k2.7-code-highspeed",
        ),
        "siliconflow" => (
            "https://api.siliconflow.cn/v1/chat/completions",
            "moonshotai/Kimi-K2.6",
        ),
        "siliconflow-cn" => (
            "https://api.siliconflow.cn/v1/chat/completions",
            "baidu/ERNIE-4.5-300B-A47B",
        ),
        "alibaba" | "alibaba-cn" => (
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            "qwen3-coder-plus",
        ),
        "minimax" => (
            "https://api.minimax.chat/v1/chat/completions",
            "MiniMax-M2.1",
        ),
        "minimax-cn" => (
            "https://api.minimax.chat/v1/chat/completions",
            "MiniMax-M2.1",
        ),
        "groq" => (
            "https://api.groq.com/openai/v1/chat/completions",
            "llama-3.3-70b-versatile",
        ),
        "together" | "togetherai" => (
            "https://api.together.xyz/v1/chat/completions",
            "meta-llama/Llama-3.3-70B-Instruct",
        ),
        "deepinfra" => (
            "https://api.deepinfra.com/v1/openai/chat/completions",
            "meta-llama/Llama-3.3-70B-Instruct",
        ),
        "fireworks-ai" => (
            "https://api.fireworks.ai/inference/v1/chat/completions",
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
        ),
        "stepfun" => ("https://api.stepfun.ai/v1/chat/completions", "step-1-32k"),
        "xai" => ("https://api.x.ai/v1/chat/completions", "grok-4"),
        "perplexity" => ("https://api.perplexity.ai/chat/completions", "sonar-pro"),
        "mistral" => (
            "https://api.mistral.ai/v1/chat/completions",
            "codestral-latest",
        ),
        "cohere" => ("https://api.cohere.ai/v1/chat/completions", "command-a"),
        "cerebras" => ("https://api.cerebras.ai/v1/chat/completions", "llama3.1-8b"),
        "nebius" => (
            "https://api.studio.nebius.ai/v1/chat/completions",
            "meta-llama/Meta-Llama-3.3-70B-Instruct",
        ),
        "novita-ai" => (
            "https://api.novita.ai/v1/chat/completions",
            "meta-llama/llama-3.1-8b-instruct",
        ),
        "friendli" => (
            "https://inference.friendli.ai/openai/v1/chat/completions",
            "google/gemma-4-31B-it",
        ),
        "nvidia" => (
            "https://integrate.api.nvidia.com/v1/chat/completions",
            "meta/llama-3.3-70b-instruct",
        ),
        "sambanova" => (
            "https://api.sambanova.ai/v1/chat/completions",
            "Meta-Llama-3.3-70B-Instruct",
        ),
        _ => ("https://api.openai.com/v1/chat/completions", "gpt-4o"),
    }
}

/// API key env vars per provider (models.dev + Hermes overlays).
pub fn api_key_envs(t: &str) -> &[&str] {
    match t {
        "openai" => &["OPENAI_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY", "ANTHROPIC_TOKEN"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "glm" | "zhipu" | "zhipuai" => &["ZHIPUAI_API_KEY", "GLM_API_KEY"],
        "zai" => &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
        "kimi" | "moonshot" | "moonshotai" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "kimi-cn" | "moonshotai-cn" => &["KIMI_CN_API_KEY"],
        "siliconflow" | "siliconflow-cn" => &["SILICONFLOW_API_KEY"],
        "alibaba" | "alibaba-cn" => &["ALIBABA_API_KEY", "DASHSCOPE_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "together" | "togetherai" => &["TOGETHER_API_KEY"],
        "deepinfra" => &["DEEPINFRA_API_KEY"],
        "fireworks-ai" => &["FIREWORKS_API_KEY"],
        "stepfun" => &["STEPFUN_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "perplexity" => &["PERPLEXITY_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "cohere" => &["COHERE_API_KEY"],
        "cerebras" => &["CEREBRAS_API_KEY"],
        "nebius" => &["NEBIUS_API_KEY"],
        "novita-ai" => &["NOVITA_API_KEY"],
        "friendli" => &["FRIENDLI_API_KEY"],
        "nvidia" => &["NVIDIA_API_KEY"],
        "sambanova" => &["SAMBA_API_KEY", "SAMBANOVA_API_KEY"],
        _ => &[],
    }
}
