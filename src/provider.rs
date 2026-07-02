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

pub trait Provider {
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
            if let Some(msg) = &chunk.message {
                if let Some(content) = &msg.content {
                    if !content.is_empty() {
                        return ContentEvent::Token(content.clone());
                    }
                }
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
            if let Some(usage) = chunk.usage {
                if let Some(choices) = &chunk.choices {
                    if choices.is_empty() {
                        return ContentEvent::Done(usage.completion_tokens);
                    }
                }
            }
            if let Some(choices) = chunk.choices {
                for choice in choices {
                    if let Some(content) = choice.delta.content {
                        if !content.is_empty() {
                            return ContentEvent::Token(content);
                        }
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
                    if let Some(text) = delta.text {
                        if !text.is_empty() {
                            return ContentEvent::Token(text);
                        }
                    }
                    ContentEvent::None
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

// ── Factory ─────────────────────────────────────────────────────────────────

pub fn from_type(t: &str) -> Box<dyn Provider> {
    match t {
        "ollama" => Box::new(Ollama),
        "openai" => Box::new(OpenAI),
        "anthropic" => Box::new(Anthropic),
        "gemini" => Box::new(OpenAI), // ponytail: Gemini uses OpenAI-compatible streaming format
        _ => Box::new(OpenAI),        // ponytail: default to OpenAI-compatible
    }
}
