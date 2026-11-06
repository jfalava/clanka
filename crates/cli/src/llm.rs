//! The writer model: any OpenAI-compatible `/chat/completions` endpoint with
//! tool calls. The chat model the user talks to is the host's business; this
//! is only the model the companion reflects and sleeps with.

use crate::setup::WriterConfig;
use clanka_core::deps::{Llm, LlmRequest, LlmResponse, Role, ToolCall};
use clanka_core::{Error, Result};
use serde_json::{Value, json};
use std::time::Duration;

/// Most a reflection's replies run to; tool arguments are short.
const MAX_TOKENS: u32 = 2048;

pub struct OpenAiWriter {
    config: WriterConfig,
    agent: ureq::Agent,
}

impl OpenAiWriter {
    pub fn new(config: WriterConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(180)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { config, agent }
    }
}

impl Llm for OpenAiWriter {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let key = std::env::var(&self.config.api_key_env).map_err(|_| {
            Error::Llm(format!(
                "the writer's API key isn't set: export {}",
                self.config.api_key_env
            ))
        })?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .send(body(&self.config.model, request)?.to_string())
            .map_err(|e| Error::Llm(format!("{url}: {e}")))?;
        let status = response.status();
        let text = response
            .body_mut()
            .with_config()
            .limit(16 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| Error::Llm(format!("{url}: {e}")))?;
        if !status.is_success() {
            return Err(Error::Llm(format!("{url}: HTTP {status}: {text}")));
        }
        parse(&text)
    }
}

fn body(model: &str, request: &LlmRequest) -> Result<Value> {
    let mut messages = vec![json!({ "role": "system", "content": request.system })];
    for m in &request.messages {
        messages.push(match m.role {
            Role::User => json!({ "role": "user", "content": m.content }),
            Role::Tool => json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": m.content,
            }),
            Role::Assistant if m.tool_calls.is_empty() => {
                json!({ "role": "assistant", "content": m.content })
            }
            Role::Assistant => json!({
                "role": "assistant",
                "content": if m.content.is_empty() { Value::Null } else { m.content.clone().into() },
                "tool_calls": m.tool_calls.iter().map(|c| json!({
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments },
                })).collect::<Vec<_>>(),
            }),
        });
    }
    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": request.temperature,
        "max_tokens": MAX_TOKENS,
    });
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|t| {
                let parameters: Value = serde_json::from_str(&t.parameters)
                    .map_err(|e| Error::Llm(format!("tool {} schema: {e}", t.name)))?;
                Ok(json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": parameters,
                    },
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        body["tools"] = tools.into();
    }
    Ok(body)
}

fn parse(text: &str) -> Result<LlmResponse> {
    let bad = |what: &str| Error::Llm(format!("unexpected response ({what}): {text}"));
    let value: Value = serde_json::from_str(text).map_err(|_| bad("not JSON"))?;
    let message = &value["choices"][0]["message"];
    if message.is_null() {
        return Err(bad("no choices[0].message"));
    }
    let tool_calls = match message["tool_calls"].as_array() {
        None => Vec::new(),
        Some(calls) => calls
            .iter()
            .map(|c| {
                let f = &c["function"];
                Ok(ToolCall {
                    id: c["id"]
                        .as_str()
                        .ok_or_else(|| bad("tool call without id"))?
                        .into(),
                    name: f["name"]
                        .as_str()
                        .ok_or_else(|| bad("tool call without name"))?
                        .into(),
                    // Some providers send arguments as an object instead of a string.
                    arguments: match &f["arguments"] {
                        Value::String(s) => s.clone(),
                        Value::Null => "{}".into(),
                        other => other.to_string(),
                    },
                })
            })
            .collect::<Result<_>>()?,
    };
    Ok(LlmResponse {
        text: message["content"].as_str().unwrap_or_default().into(),
        tool_calls,
    })
}

/// Stands in when no writer is configured, so the error says how to fix it.
pub struct NoWriter;

impl Llm for NoWriter {
    fn complete(&self, _: &LlmRequest) -> Result<LlmResponse> {
        Err(Error::Llm(
            "no writer model configured; add a [writer] section to ~/.config/clanka/config.toml"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clanka_core::deps::{LlmMessage, ToolSpec};

    #[test]
    fn a_tool_round_trips_in_openai_shape() {
        let request = LlmRequest {
            system: "sys".into(),
            messages: vec![
                LlmMessage::user("hi"),
                LlmMessage {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "set_mood".into(),
                        arguments: "{\"valence\":0.2}".into(),
                    }],
                    tool_call_id: None,
                },
                LlmMessage {
                    role: Role::Tool,
                    content: "ok".into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("c1".into()),
                },
            ],
            tools: vec![ToolSpec {
                name: "set_mood".into(),
                description: "d".into(),
                parameters: "{\"type\":\"object\"}".into(),
            }],
            temperature: 0.2,
        };
        let body = body("m", &request).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["content"], Value::Null);
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"],
            "set_mood"
        );
        assert_eq!(body["messages"][3]["tool_call_id"], "c1");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");

        let reply = parse(
            r#"{"choices":[{"message":{"content":null,"tool_calls":[
                {"id":"c2","type":"function","function":{"name":"remember","arguments":{"a":1}}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(reply.text, "");
        assert_eq!(reply.tool_calls[0].id, "c2");
        assert_eq!(reply.tool_calls[0].arguments, "{\"a\":1}");
    }
}
