//! Tool calls for a model that only returns text, such as a host's plugin
//! model API with no function calling. The model answers with one JSON object
//! of actions. Each action becomes the same [`ToolCall`] a native tool call
//! would, and runs through the same guarded handlers. A reply that isn't
//! exactly that shape runs nothing at all.

use crate::deps::{ToolCall, ToolSpec};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Most actions one reply may carry. More is a malformed reply, not a batch.
pub const MAX_ACTIONS: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    tool: String,
    arguments: Map<String, Value>,
}

/// What the system prompt adds so the model knows the reply format and the tools.
pub fn protocol(tools: &[ToolSpec]) -> String {
    let mut out = String::from(
        "You have no function-calling interface here. To use tools, reply with ONLY one JSON \
         object, with no other text and no code fence:\n\
         {\"actions\": [{\"tool\": \"<tool name>\", \"arguments\": {<arguments>}}]}\n\
         Put every action you want in this one reply; they run in order. You hear back only \
         if one fails, so you can fix it. When there is nothing to do, reply \
         {\"actions\": []}.\n\nTools:",
    );
    for t in tools {
        out.push_str(&format!(
            "\n- {}: {}\n  arguments (JSON Schema): {}",
            t.name, t.description, t.parameters
        ));
    }
    out
}

/// The reply's actions as tool calls, or why the whole reply was refused.
/// A single wrong action refuses all of them: a malformed reply runs nothing.
pub fn parse(text: &str, tools: &[ToolSpec]) -> Result<Vec<ToolCall>, String> {
    let text = strip_fence(text.trim());
    let reply: Reply = serde_json::from_str(text).map_err(|e| {
        format!("the reply must be exactly {{\"actions\": [...]}} and nothing else ({e})")
    })?;
    if reply.actions.len() > MAX_ACTIONS {
        return Err(format!(
            "{} actions; at most {MAX_ACTIONS} per reply",
            reply.actions.len()
        ));
    }
    reply
        .actions
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            if !tools.iter().any(|t| t.name == a.tool) {
                return Err(format!("there is no tool called {:?}", a.tool));
            }
            Ok(ToolCall {
                id: format!("action-{i}"),
                name: a.tool,
                arguments: Value::Object(a.arguments).to_string(),
            })
        })
        .collect()
}

/// A whole reply wrapped in a Markdown code fence is accepted; nothing else around it is.
fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some((_lang, body)) = rest.split_once('\n') else {
        return text;
    };
    body.trim_end().strip_suffix("```").map_or(text, str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "leave_note".into(),
            description: "d".into(),
            parameters: "{}".into(),
        }]
    }

    #[test]
    fn only_the_exact_shape_is_accepted() {
        let ok = r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "hi"}}]}"#;
        let calls = parse(ok, &tools()).unwrap();
        assert_eq!(
            (calls[0].name.as_str(), calls[0].arguments.as_str()),
            ("leave_note", r#"{"note":"hi"}"#)
        );
        assert_eq!(
            parse(&format!("```json\n{ok}\n```"), &tools())
                .unwrap()
                .len(),
            1
        );
        assert!(parse(r#"{"actions": []}"#, &tools()).unwrap().is_empty());

        for bad in [
            &format!("Sure! {ok}") as &str,
            r#"[{"tool": "leave_note", "arguments": {}}]"#,
            r#"{"actions": [{"tool": "leave_note"}]}"#,
            r#"{"actions": [{"tool": "leave_note", "arguments": "note=hi"}]}"#,
            r#"{"actions": [{"tool": "leave_note", "arguments": {}, "why": "x"}]}"#,
            r#"{"actions": [], "done": true}"#,
        ] {
            assert!(parse(bad, &tools()).is_err(), "accepted {bad}");
        }
        let many = format!(
            r#"{{"actions": [{}]}}"#,
            vec![r#"{"tool": "leave_note", "arguments": {}}"#; MAX_ACTIONS + 1].join(",")
        );
        assert!(parse(&many, &tools()).is_err());
    }
}
