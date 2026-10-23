use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::attachments::Attachment;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    DeepSeek,
}

impl Provider {
    pub const ALL: [Self; 5] = [Self::Ollama, Self::OpenAI, Self::Anthropic, Self::Gemini, Self::DeepSeek];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::OpenAI => "OpenAI / compatible",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Gemini",
            Self::DeepSeek => "DeepSeek",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::Ollama => "http://127.0.0.1:11434",
            Self::OpenAI => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::DeepSeek => "https://api.deepseek.com",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => "qwen3:8b",
            Self::OpenAI => "gpt-4.1-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::Gemini => "gemini-2.5-flash",
            Self::DeepSeek => "deepseek-flash",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub provider: Provider,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
}

pub struct Conversation {
    settings: Settings,
    client: Client,
    messages: Vec<Value>,
    instruction: Option<String>,
}

impl Conversation {
    #[cfg(test)]
    pub fn new(settings: Settings, history: &[ChatMessage]) -> Result<Self> {
        Self::new_with_instruction(settings, history, None)
    }

    pub fn new_with_instruction(settings: Settings, history: &[ChatMessage],
        instruction: Option<String>) -> Result<Self> {
        let client = Client::builder().timeout(std::time::Duration::from_secs(180)).build()?;
        if settings.provider == Provider::DeepSeek && settings.model != "deepseek-flash"
            && history.iter().flat_map(|m| &m.attachments).any(Attachment::is_image) {
            bail!("DeepSeek image attachments require deepseek-flash; choose Flash or start a new chat");
        }
        let mut messages: Vec<Value> = Vec::new();
        if matches!(settings.provider, Provider::Ollama | Provider::OpenAI | Provider::DeepSeek) {
            if let Some(text) = &instruction {
                messages.push(json!({"role":"system", "content":text}));
            }
        }
        messages.extend(history.iter().map(|m| message_to_wire(settings.provider, m)));
        Ok(Self { settings, client, messages, instruction })
    }

    pub fn request_visible_answer(&mut self) {
        let message = ChatMessage { role: "user".into(),
            content: "Your previous response had no visible answer or tool call. Give a concise answer to the user's last request, or call an appropriate available tool now. Do not output only internal reasoning.".into(),
            attachments: vec![] };
        self.messages.push(message_to_wire(self.settings.provider, &message));
    }

    pub async fn next(&mut self, tools: &[ToolSpec]) -> Result<Reply> {
        let s = &self.settings;
        let base = s.base_url.trim_end_matches('/');
        let req = match s.provider {
            Provider::Ollama => {
                let mut body = json!({"model": s.model, "messages": self.messages, "stream": false, "think": false});
                if !tools.is_empty() { body["tools"] = openai_tools(tools); }
                self.client.post(format!("{base}/api/chat")).json(&body)
            }
            Provider::OpenAI | Provider::DeepSeek => {
                let mut body = json!({"model": s.model, "messages": self.messages});
                if !tools.is_empty() { body["tools"] = openai_tools(tools); }
                self.client.post(format!("{base}/chat/completions"))
                    .bearer_auth(&s.api_key).json(&body)
            }
            Provider::Anthropic => {
                let mut body = json!({"model": s.model, "max_tokens": 4096, "messages": self.messages});
                if let Some(instruction) = &self.instruction { body["system"] = json!(instruction); }
                if !tools.is_empty() {
                    body["tools"] = Value::Array(tools.iter().map(|t| json!({
                        "name": t.name, "description": t.description, "input_schema": t.parameters
                    })).collect());
                }
                self.client.post(format!("{base}/messages"))
                    .header("x-api-key", &s.api_key)
                    .header("anthropic-version", "2023-06-01").json(&body)
            }
            Provider::Gemini => {
                let mut body = json!({"contents": self.messages});
                if let Some(instruction) = &self.instruction {
                    body["systemInstruction"] = json!({"parts":[{"text":instruction}]});
                }
                if !tools.is_empty() {
                    body["tools"] = json!([{"functionDeclarations": tools.iter().map(|t| json!({
                        "name": t.name, "description": t.description, "parameters": t.parameters
                    })).collect::<Vec<_>>() }]);
                }
                self.client.post(format!("{base}/models/{}:generateContent", s.model))
                    .header("x-goog-api-key", &s.api_key).json(&body)
            }
        };
        let response = req.send().await.context("Provider connection failed")?;
        let status = response.status();
        let body: Value = response.json().await.context("Provider returned invalid JSON")?;
        if !status.is_success() {
            bail!("Provider HTTP {status}: {}", truncate(&body.to_string(), 700));
        }
        self.parse_reply(body)
    }

    fn parse_reply(&mut self, body: Value) -> Result<Reply> {
        let mut calls = Vec::new();
        let mut text;
        match self.settings.provider {
            Provider::Ollama | Provider::OpenAI | Provider::DeepSeek => {
                let message = if self.settings.provider == Provider::Ollama {
                    body.get("message")
                } else {
                    body.pointer("/choices/0/message")
                }.cloned().context("No assistant message in provider response")?;
                text = message.get("content").and_then(Value::as_str).unwrap_or("").to_owned();
                if self.settings.provider == Provider::Ollama {
                    if let Some(end) = text.find("</think>") {
                        text = text[end + "</think>".len()..].trim().to_owned();
                    }
                }
                if let Some(items) = message.get("tool_calls").and_then(Value::as_array) {
                    for (index, item) in items.iter().enumerate() {
                        let function = &item["function"];
                        let arguments = match &function["arguments"] {
                            Value::String(s) => serde_json::from_str(s).context("Invalid tool arguments")?,
                            Value::Null => json!({}),
                            v => v.clone(),
                        };
                        calls.push(ToolCall {
                            id: item.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                            name: function["name"].as_str().context("Tool call without name")?.to_owned(),
                            arguments,
                        });
                        if calls[index].id.is_empty() { calls[index].id = format!("call_{index}"); }
                    }
                }
                self.messages.push(message);
            }
            Provider::Anthropic => {
                let content = body["content"].as_array().context("No content in Anthropic response")?;
                text = content.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
                for block in content {
                    if block["type"] == "tool_use" {
                        calls.push(ToolCall {
                            id: block["id"].as_str().context("Tool use without ID")?.to_owned(),
                            name: block["name"].as_str().context("Tool use without name")?.to_owned(),
                            arguments: block["input"].clone(),
                        });
                    }
                }
                self.messages.push(json!({"role":"assistant", "content":content}));
            }
            Provider::Gemini => {
                let content = body.pointer("/candidates/0/content").cloned().context("No Gemini candidate")?;
                let parts = content["parts"].as_array().context("No Gemini content parts")?;
                text = parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
                for (index, part) in parts.iter().enumerate() {
                    if let Some(function) = part.get("functionCall") {
                        calls.push(ToolCall {
                            id: function.get("id").and_then(Value::as_str).unwrap_or(&format!("call_{index}")).to_owned(),
                            name: function["name"].as_str().context("Function call without name")?.to_owned(),
                            arguments: function.get("args").cloned().unwrap_or_else(|| json!({})),
                        });
                    }
                }
                self.messages.push(content);
            }
        }
        Ok(Reply { text, calls })
    }

    pub fn add_results(&mut self, results: &[(ToolCall, String)]) {
        match self.settings.provider {
            Provider::Ollama => for (call, output) in results {
                self.messages.push(json!({"role":"tool", "tool_name":call.name, "content":output}));
            },
            Provider::OpenAI | Provider::DeepSeek => for (call, output) in results {
                self.messages.push(json!({"role":"tool", "tool_call_id":call.id, "content":output}));
            },
            Provider::Anthropic => {
                let blocks: Vec<_> = results.iter().map(|(call, output)| json!({
                    "type":"tool_result", "tool_use_id":call.id, "content":output
                })).collect();
                self.messages.push(json!({"role":"user", "content":blocks}));
            }
            Provider::Gemini => {
                let parts: Vec<_> = results.iter().map(|(call, output)| {
                    let mut response = json!({"name":call.name, "response":{"result":output}});
                    if !call.id.starts_with("call_") { response["id"] = json!(call.id); }
                    json!({"functionResponse":response})
                }).collect();
                self.messages.push(json!({"role":"user", "parts":parts}));
            }
        }
    }
}

fn message_to_wire(provider: Provider, message: &ChatMessage) -> Value {
    let mut text = message.content.clone();
    let mut images = Vec::new();
    for attachment in &message.attachments {
        match attachment {
            Attachment::Text { name, text: file_text } => {
                text.push_str(&format!("\n\nAttached file {}:\n{}\nEnd of attached file.\n",
                    serde_json::to_string(name).unwrap_or_default(), file_text));
            }
            Attachment::Image { name, mime, data, .. } => {
                text.push_str(&format!("\nAttached image: {}.\n",
                    serde_json::to_string(name).unwrap_or_default()));
                images.push((mime, data));
            }
        }
    }
    match provider {
        Provider::Ollama if !images.is_empty() => json!({
            "role": message.role, "content": text,
            "images": images.iter().map(|(_, data)| *data).collect::<Vec<_>>()
        }),
        Provider::Ollama => json!({"role": message.role, "content": text}),
        Provider::OpenAI | Provider::DeepSeek if !images.is_empty() => {
            let mut parts = vec![json!({"type":"text", "text":text})];
            parts.extend(images.iter().map(|(mime, data)| json!({
                "type":"image_url", "image_url":{"url":format!("data:{mime};base64,{data}")}
            })));
            json!({"role":message.role, "content":parts})
        }
        Provider::OpenAI | Provider::DeepSeek => json!({"role":message.role, "content":text}),
        Provider::Anthropic if !images.is_empty() => {
            let mut parts = vec![json!({"type":"text", "text":text})];
            parts.extend(images.iter().map(|(mime, data)| json!({
                "type":"image", "source":{"type":"base64", "media_type":mime, "data":data}
            })));
            json!({"role":message.role, "content":parts})
        }
        Provider::Anthropic => json!({"role":message.role, "content":text}),
        Provider::Gemini => {
            let mut parts = vec![json!({"text":text})];
            parts.extend(images.iter().map(|(mime, data)| json!({
                "inlineData":{"mimeType":mime, "data":data}
            })));
            json!({
                "role":if message.role == "assistant" { "model" } else { "user" },
                "parts":parts
            })
        }
    }
}

fn openai_tools(tools: &[ToolSpec]) -> Value {
    Value::Array(tools.iter().map(|t| json!({
        "type":"function", "function": {
            "name":t.name, "description":t.description, "parameters":t.parameters
        }
    })).collect())
}

fn key_check_request(client: &Client, settings: &Settings) -> Result<reqwest::Request> {
    let base = settings.base_url.trim_end_matches('/');
    let request = client.get(format!("{base}/models"));
    let request = match settings.provider {
        Provider::OpenAI | Provider::DeepSeek => request.bearer_auth(&settings.api_key),
        Provider::Anthropic => request
            .header("x-api-key", &settings.api_key)
            .header("anthropic-version", "2023-06-01"),
        Provider::Gemini => request.header("x-goog-api-key", &settings.api_key),
        Provider::Ollama => bail!("Ollama does not use an API key"),
    };
    Ok(request.build()?)
}

pub async fn check_api_key(settings: Settings) -> Result<()> {
    if settings.api_key.trim().is_empty() { bail!("Enter an API key first"); }
    let client = Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    let request = key_check_request(&client, &settings)?;
    let response = client.execute(request).await.context("Cannot reach the provider")?;
    match response.status().as_u16() {
        200..=299 => {
            let body: Value = response.json().await.context("Could not read the model list")?;
            let field = if settings.provider == Provider::Gemini { "models" } else { "data" };
            if !body[field].is_array() { bail!("Unexpected model list response"); }
            Ok(())
        }
        401 => bail!("API key rejected (HTTP 401)"),
        403 => bail!("Access denied (HTTP 403); check key permissions"),
        404 => bail!("Models endpoint not found (HTTP 404); check Endpoint"),
        429 => bail!("Rate limit reached (HTTP 429); key could not be verified"),
        code => bail!("Provider returned HTTP {code}"),
    }
}

pub async fn ollama_models(url: &str) -> Result<Vec<String>> {
    let response = Client::new().get(format!("{}/api/tags", url.trim_end_matches('/')))
        .send().await.context("Cannot reach Ollama")?.error_for_status()?;
    let data: Value = response.json().await?;
    let names = data["models"].as_array().context("Ollama returned no model list")?
        .iter().filter_map(|m| m["name"].as_str().map(str::to_owned)).collect();
    Ok(names)
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(provider: Provider) -> Conversation {
        Conversation::new(Settings {
            provider, model: "test".into(), base_url: "http://localhost".into(), api_key: String::new(),
        }, &[ChatMessage { role: "user".into(), content: "hello".into(), attachments: vec![] }]).unwrap()
    }

    #[test]
    fn ollama_tool_call_and_thinking_are_parsed() {
        let mut chat = conversation(Provider::Ollama);
        let reply = chat.parse_reply(json!({"message": {
            "role":"assistant", "content":"<think>reason</think>Done",
            "tool_calls":[{"function":{"name":"files_read", "arguments":{"path":"a"}}}]
        }})).unwrap();
        assert_eq!(reply.text, "Done");
        assert_eq!(reply.calls[0].name, "files_read");
        chat.add_results(&[(reply.calls[0].clone(), "content".into())]);
        assert_eq!(chat.messages.last().unwrap()["tool_name"], "files_read");
    }

    #[test]
    fn openai_tool_arguments_and_result_id_are_preserved() {
        let mut chat = conversation(Provider::OpenAI);
        let reply = chat.parse_reply(json!({"choices":[{"message": {
            "role":"assistant", "content":null,
            "tool_calls":[{"id":"call_123", "type":"function", "function":{"name":"files_read", "arguments":"{\"path\":\"a\"}"}}]
        }}]})).unwrap();
        assert_eq!(reply.calls[0].arguments["path"], "a");
        chat.add_results(&[(reply.calls[0].clone(), "content".into())]);
        assert_eq!(chat.messages.last().unwrap()["tool_call_id"], "call_123");
    }

    #[test]
    fn anthropic_tool_use_and_result_are_preserved() {
        let mut chat = conversation(Provider::Anthropic);
        let reply = chat.parse_reply(json!({"content":[
            {"type":"text", "text":"Checking"},
            {"type":"tool_use", "id":"toolu_123", "name":"files_read", "input":{"path":"a"}}
        ]})).unwrap();
        assert_eq!(reply.text, "Checking");
        chat.add_results(&[(reply.calls[0].clone(), "content".into())]);
        assert_eq!(chat.messages.last().unwrap()["content"][0]["tool_use_id"], "toolu_123");
    }

    #[test]
    fn gemini_function_call_and_result_are_preserved() {
        let mut chat = conversation(Provider::Gemini);
        let reply = chat.parse_reply(json!({"candidates":[{"content":{
            "role":"model", "parts":[{"functionCall":{"id":"fc_123", "name":"files_read", "args":{"path":"a"}}}]
        }}]})).unwrap();
        assert_eq!(reply.calls[0].arguments["path"], "a");
        chat.add_results(&[(reply.calls[0].clone(), "content".into())]);
        assert_eq!(chat.messages.last().unwrap()["parts"][0]["functionResponse"]["id"], "fc_123");
    }

    #[test]
    fn deepseek_tool_call_preserves_reasoning_and_result_id() {
        let mut chat = conversation(Provider::DeepSeek);
        let reply = chat.parse_reply(json!({"choices":[{"message": {
            "role":"assistant", "content":null, "reasoning_content":"I should use a tool.",
            "tool_calls":[{"id":"call_ds", "type":"function", "function":{"name":"graph_cad_nodes", "arguments":"{}"}}]
        }}]})).unwrap();
        assert_eq!(reply.calls[0].name, "graph_cad_nodes");
        assert_eq!(chat.messages.last().unwrap()["reasoning_content"], "I should use a tool.");
        chat.add_results(&[(reply.calls[0].clone(), "nodes".into())]);
        assert_eq!(chat.messages.last().unwrap()["tool_call_id"], "call_ds");
    }

    #[test]
    fn key_check_sends_secrets_in_headers_only() {
        let client = Client::new();
        for provider in [Provider::OpenAI, Provider::Anthropic, Provider::Gemini, Provider::DeepSeek] {
            let settings = Settings {
                provider, model: "test".into(), base_url: "https://example.test/v1".into(),
                api_key: "secret-for-test".into(),
            };
            let request = key_check_request(&client, &settings).unwrap();
            assert_eq!(request.url().as_str(), "https://example.test/v1/models");
            assert!(!request.url().as_str().contains(&settings.api_key));
            let header = match provider {
                Provider::Gemini => "x-goog-api-key",
                Provider::Anthropic => "x-api-key",
                _ => "authorization",
            };
            assert!(request.headers().get(header).is_some());
        }
    }

    #[test]
    fn attachments_use_each_provider_image_format() {
        let message = ChatMessage {
            role: "user".into(), content: "Explain these".into(),
            attachments: vec![
                Attachment::Text { name: "notes.md".into(), text: "hello".into() },
                Attachment::Image { name: "plot.png".into(), mime: "image/png".into(),
                    data: "aGVsbG8=".into(), bytes: 5 },
            ],
        };
        let ollama = message_to_wire(Provider::Ollama, &message);
        assert_eq!(ollama["images"][0], "aGVsbG8=");
        assert!(ollama["content"].as_str().unwrap().contains("notes.md"));
        let openai = message_to_wire(Provider::OpenAI, &message);
        assert_eq!(openai["content"][1]["image_url"]["url"], "data:image/png;base64,aGVsbG8=");
        let anthropic = message_to_wire(Provider::Anthropic, &message);
        assert_eq!(anthropic["content"][1]["source"]["media_type"], "image/png");
        let gemini = message_to_wire(Provider::Gemini, &message);
        assert_eq!(gemini["parts"][1]["inlineData"]["data"], "aGVsbG8=");
    }
}
