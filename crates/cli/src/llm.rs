//! The companion's model over any OpenAI-compatible `/chat/completions`
//! endpoint with tool calls. The same client serves conversation (`clanka
//! chat`) and memory writing (the writer, sleep and the doctor).

use crate::setup::ModelConfig;
use clanka_core::deps::{Llm, LlmRequest, LlmResponse, Role, ToolCall};
use clanka_core::{Error, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

/// Most one reply or reflection round runs to.
const MAX_TOKENS: u32 = 2048;

/// Tries per request: the first and up to three more after a failure that may
/// pass (a dropped connection, a rate limit, a provider error).
const ATTEMPTS: u32 = 4;

/// Longest a provider's `Retry-After` is waited out. A turn is waiting: asked
/// to come back later than this, the request fails at once.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Longest one request may take from the first try to the end of the reply,
/// retries and the waits between them included: a turn is waiting on it.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Most of a response that isn't an event stream that is read.
const MAX_BODY: usize = 16 * 1024 * 1024;

pub struct OpenAiModel {
    config: ModelConfig,
    agent: ureq::Agent,
    /// Wait before the second try; it doubles for each one after.
    backoff: Duration,
    /// See [`TIMEOUT`].
    timeout: Duration,
}

impl OpenAiModel {
    pub fn new(config: ModelConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            config,
            agent,
            backoff: Duration::from_secs(1),
            timeout: TIMEOUT,
        }
    }

    /// Posts `body` and returns the successful response, trying again after
    /// failures that may pass. A request the provider rejects (a bad key, an
    /// unknown model) fails at once.
    fn send(&self, body: &Value) -> Result<ureq::http::Response<ureq::Body>> {
        let key = std::env::var(&self.config.api_key_env).map_err(|_| {
            Error::Llm(format!(
                "the companion model's API key isn't set: export {}",
                self.config.api_key_env
            ))
        })?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = body.to_string();
        let started = Instant::now();
        let mut attempt = 1;
        loop {
            // Each try gets what is left of the one limit, not a full one of
            // its own: four slow failures in a row took four times as long.
            let left = self.timeout.saturating_sub(started.elapsed());
            let sent = self
                .agent
                .post(&url)
                .config()
                .timeout_global(Some(left))
                .build()
                .header("Authorization", &format!("Bearer {key}"))
                .header("Content-Type", "application/json")
                .send(&body);
            let last = attempt == ATTEMPTS;
            let (error, retry_after) = match sent {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(mut response) => {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let text = read(&mut response).unwrap_or_default();
                    let error = Error::Llm(format!("{url}: HTTP {status}: {text}"));
                    if last
                        || !retryable_status(status.as_u16())
                        || retry_after.is_some_and(|asked| asked > MAX_RETRY_AFTER)
                    {
                        return Err(error);
                    }
                    (error, retry_after)
                }
                Err(e) => {
                    let retry = retryable_error(&e);
                    let error = Error::Llm(format!("{url}: {e}"));
                    if last || !retry {
                        return Err(error);
                    }
                    (error, None)
                }
            };
            let wait = delay(self.backoff, attempt, retry_after);
            if started.elapsed() + wait >= self.timeout {
                return Err(error);
            }
            eprintln!(
                "clanka: {error}; trying again in {:.1}s",
                wait.as_secs_f32()
            );
            std::thread::sleep(wait);
            attempt += 1;
        }
    }
}

fn read(response: &mut ureq::http::Response<ureq::Body>) -> Result<String> {
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY as u64)
        .read_to_string()
        .map_err(|e| Error::Llm(format!("reading the model's response: {e}")))
}

/// Rate limits, request timeouts and provider-side failures.
fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..600).contains(&status)
}

/// Failures to reach the provider. A request that ran into the overall
/// [`TIMEOUT`] is not tried again: the user has waited long enough.
fn retryable_error(e: &ureq::Error) -> bool {
    use ureq::Error::{ConnectionFailed, HostNotFound, Io, Timeout};
    match e {
        Io(_) | ConnectionFailed | HostNotFound => true,
        Timeout(t) => !matches!(t, ureq::Timeout::Global),
        _ => false,
    }
}

/// The wait after failed try number `attempt` (from 1): what the provider
/// asked for when it said, else the backoff doubled for each try so far.
fn delay(backoff: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| backoff * 2u32.pow(attempt - 1))
}

impl Llm for OpenAiModel {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let mut response = self.send(&body(&self.config.model, request)?)?;
        parse(&read(&mut response)?)
    }

    /// Server-sent events for plain replies. A request with tools is answered
    /// whole: tool calls are bookkeeping nobody watches arrive.
    fn stream(&self, request: &LlmRequest, on_text: &mut dyn FnMut(&str)) -> Result<LlmResponse> {
        if !request.tools.is_empty() {
            let response = self.complete(request)?;
            if !response.text.is_empty() {
                on_text(&response.text);
            }
            return Ok(response);
        }
        let mut body = body(&self.config.model, request)?;
        body["stream"] = true.into();
        let response = self.send(&body)?;
        let mut text = String::new();
        // What came before any event: the whole body, when it is no stream.
        let mut plain = String::new();
        let (mut events, mut finished) = (false, false);
        for line in BufReader::new(response.into_body().into_reader()).lines() {
            let line = line.map_err(|e| Error::Llm(format!("reading the model's reply: {e}")))?;
            if !events && !line.starts_with("data:") {
                if plain.len() + line.len() > MAX_BODY {
                    return Err(Error::Llm("the model's response is too large".into()));
                }
                plain.push_str(&line);
                plain.push('\n');
                continue;
            }
            events = true;
            match event(&line)? {
                Event::Text(piece) => {
                    on_text(&piece);
                    text.push_str(&piece);
                }
                Event::Finished(piece) => {
                    if !piece.is_empty() {
                        on_text(&piece);
                        text.push_str(&piece);
                    }
                    finished = true;
                }
                Event::Other => {}
                Event::Done => {
                    finished = true;
                    break;
                }
            }
        }
        // An endpoint that ignores `stream` answers whole, and some send an
        // error with status 200: both read like any unstreamed response.
        if !events {
            let response = parse(plain.trim())?;
            if !response.text.is_empty() {
                on_text(&response.text);
            }
            return Ok(response);
        }
        // A connection closed mid-reply looks like the end of the stream.
        if !finished {
            return Err(Error::Llm(
                "the model's reply was cut off before it finished".into(),
            ));
        }
        Ok(LlmResponse {
            text,
            tool_calls: Vec::new(),
        })
    }
}

#[derive(Debug, PartialEq)]
enum Event {
    Text(String),
    /// The chunk that says why the reply ended, with any last text in it.
    Finished(String),
    /// A comment, a blank line or a chunk without text (role, usage).
    Other,
    Done,
}

/// One line of a `stream: true` response.
fn event(line: &str) -> Result<Event> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(Event::Other);
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(Event::Done);
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|_| Error::Llm(format!("unexpected stream chunk: {data}")))?;
    if !value["error"].is_null() {
        return Err(Error::Llm(format!("the model stopped mid-reply: {data}")));
    }
    let choice = &value["choices"][0];
    let piece = choice["delta"]["content"].as_str().unwrap_or_default();
    Ok(if !choice["finish_reason"].is_null() {
        Event::Finished(piece.into())
    } else if piece.is_empty() {
        Event::Other
    } else {
        Event::Text(piece.into())
    })
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

/// Stands in when no companion model is configured, so the error says how to fix it.
pub struct NoModel;

impl Llm for NoModel {
    fn complete(&self, _: &LlmRequest) -> Result<LlmResponse> {
        Err(Error::Llm(
            "no companion model configured; add a [model] section to \
             ~/.config/clanka/config.toml naming the model the companion chats with"
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

    /// A provider on localhost that answers each request with the next canned
    /// response, then stops. Returns its base URL.
    fn provider(responses: Vec<String>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                // Read the whole request, so closing doesn't reset the connection.
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request);
                    let Some(head) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length: usize = text[..head]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if n == 0 || request.len() >= head + 4 + length {
                        break;
                    }
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        url
    }

    fn http(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    fn model(base_url: String) -> OpenAiModel {
        OpenAiModel {
            backoff: Duration::from_millis(1),
            ..OpenAiModel::new(ModelConfig {
                base_url,
                model: "m".into(),
                // Any variable that is always set: the fake provider ignores the key.
                api_key_env: "PATH".into(),
            })
        }
    }

    fn hello() -> LlmRequest {
        LlmRequest {
            system: "sys".into(),
            messages: vec![LlmMessage::user("hi")],
            tools: Vec::new(),
            temperature: 0.2,
        }
    }

    const OK: &str = r#"{"choices":[{"message":{"content":"hello"}}]}"#;

    #[test]
    fn a_passing_failure_is_tried_again() {
        let url = provider(vec![
            http("503 Service Unavailable", "", "busy"),
            http("429 Too Many Requests", "Retry-After: 0\r\n", "slow down"),
            http("200 OK", "", OK),
        ]);
        assert_eq!(model(url).complete(&hello()).unwrap().text, "hello");
    }

    #[test]
    fn a_rejected_request_fails_at_once() {
        // Only one response is served: a second try would fail to connect.
        let url = provider(vec![http("401 Unauthorized", "", "bad key")]);
        let error = model(url).complete(&hello()).unwrap_err().to_string();
        assert!(
            error.contains("401") && error.contains("bad key"),
            "{error}"
        );
    }

    #[test]
    fn retries_stop_after_the_last_attempt() {
        let busy = http("500 Internal Server Error", "", "down");
        let url = provider(vec![busy; ATTEMPTS as usize]);
        let error = model(url).complete(&hello()).unwrap_err().to_string();
        assert!(error.contains("500"), "{error}");
    }

    #[test]
    fn retries_stop_when_the_next_wait_would_pass_the_time_limit() {
        // Two responses: a third try would fail to connect, not with a 500.
        let busy = http("500 Internal Server Error", "", "down");
        let model = OpenAiModel {
            backoff: Duration::from_millis(200),
            timeout: Duration::from_millis(300),
            ..model(provider(vec![busy; 2]))
        };
        let error = model.complete(&hello()).unwrap_err().to_string();
        assert!(error.contains("500"), "{error}");
    }

    #[test]
    fn the_wait_doubles_unless_the_provider_names_one() {
        let second = Duration::from_secs(1);
        assert_eq!(delay(second, 1, None), second);
        assert_eq!(delay(second, 3, None), second * 4);
        assert_eq!(delay(second, 3, Some(second * 7)), second * 7);
        assert!(retryable_status(429) && retryable_status(502));
        assert!(!retryable_status(400) && !retryable_status(404));
    }

    #[test]
    fn a_streamed_reply_arrives_in_pieces() {
        let events = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                      data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n\
                      : keep-alive\n\n\
                      data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
                      data: [DONE]\n\n";
        let url = provider(vec![http(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            events,
        )]);
        let mut pieces = Vec::new();
        let reply = model(url)
            .stream(&hello(), &mut |piece| pieces.push(piece.to_owned()))
            .unwrap();
        assert_eq!(pieces, ["hel", "lo"]);
        assert_eq!(reply.text, "hello");
        assert!(event(r#"data: {"error":{"message":"overloaded"}}"#).is_err());
    }

    fn streamed(body: &str) -> (Result<LlmResponse>, Vec<String>) {
        let url = provider(vec![http("200 OK", "", body)]);
        let mut pieces = Vec::new();
        let reply = model(url).stream(&hello(), &mut |piece| pieces.push(piece.to_owned()));
        (reply, pieces)
    }

    #[test]
    fn a_stream_must_say_it_finished_and_a_plain_answer_is_read_whole() {
        // The connection closed mid-reply: not a reply to record.
        let cut = "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n";
        let error = streamed(cut).0.unwrap_err().to_string();
        assert!(error.contains("cut off"), "{error}");

        // A finish reason ends it as well as [DONE] does, last text included.
        let ended = format!(
            "{cut}data: {{\"choices\":[{{\"delta\":{{\"content\":\"lo\"}},\
             \"finish_reason\":\"stop\"}}]}}\n\n"
        );
        let (reply, pieces) = streamed(&ended);
        assert_eq!(reply.unwrap().text, "hello");
        assert_eq!(pieces, ["hel", "lo"]);

        // An endpoint that ignores `stream`.
        let whole = r#"{"choices":[{"message":{"content":"plain reply"}}]}"#;
        let (reply, pieces) = streamed(whole);
        assert_eq!(reply.unwrap().text, "plain reply");
        assert_eq!(pieces, ["plain reply"]);

        // An error sent with status 200 is shown, not turned into an empty reply.
        let error = streamed(r#"{"error":{"message":"overloaded"}}"#)
            .0
            .unwrap_err();
        assert!(error.to_string().contains("overloaded"), "{error}");
    }

    #[test]
    fn a_provider_that_says_come_back_much_later_is_not_waited_on() {
        let later = http(
            "429 Too Many Requests",
            "Retry-After: 3600\r\n",
            "slow down",
        );
        let url = provider(vec![later]);
        let started = std::time::Instant::now();
        let error = model(url).complete(&hello()).unwrap_err().to_string();
        assert!(error.contains("429"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
