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
         {\"actions\": []}.\n\nTools, with their arguments as `name: type`. A `?` marks an \
         optional argument; a field described once means the same wherever it appears.",
    );
    let mut described: Vec<(String, String)> = Vec::new();
    for t in tools {
        out.push_str(&format!("\n- {}: {}", t.name, t.description));
        let schema: Value = serde_json::from_str(&t.parameters).unwrap_or(Value::Null);
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let Some(props) = schema["properties"].as_object() else {
            continue;
        };
        // Required arguments first, so the model sees what it can't leave out.
        let mut names: Vec<&String> = props.keys().collect();
        names.sort_by_key(|n| !required.contains(&n.as_str()));
        for name in names {
            let p = &props[name];
            let optional = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            out.push_str(&format!("\n  {name}{optional}: {}", type_of(p)));
            if let Some(d) = p["description"].as_str() {
                let key = (name.clone(), d.to_string());
                if !described.contains(&key) {
                    out.push_str(&format!(" — {d}"));
                    described.push(key);
                }
            }
        }
    }
    out
}

/// A property's JSON Schema as a short type: `string`, `"a"|"b"`, `number -1..1`, `{"<key>": string, ...}`.
fn type_of(p: &Value) -> String {
    if let Some(e) = p["enum"].as_array() {
        return e.iter().map(Value::to_string).collect::<Vec<_>>().join("|");
    }
    let ty = p["type"].as_str().unwrap_or("any");
    match ty {
        "object" => match p.get("additionalProperties") {
            Some(v) if v.is_object() => format!("{{\"<key>\": {}, ...}}", type_of(v)),
            _ => "object".into(),
        },
        "array" => format!("[{}]", type_of(&p["items"])),
        "number" | "integer" => match (p["minimum"].as_f64(), p["maximum"].as_f64()) {
            (Some(lo), Some(hi)) => format!("{ty} {lo}..{hi}"),
            _ => ty.into(),
        },
        _ => ty.into(),
    }
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

    #[test]
    fn the_protocol_keeps_every_argument_constraint_and_describes_shared_fields_once() {
        let level = r#"{"type":"string","enum":["faint","seared"],"description":"How deep."}"#;
        let spec = |name: &str, props: String, required: &str| ToolSpec {
            name: name.into(),
            description: format!("{name} it."),
            parameters: format!(
                r#"{{"type":"object","properties":{{{props}}},"required":[{required}]}}"#
            ),
        };
        let p = protocol(&[
            spec(
                "remember",
                format!(
                    r#""valence":{{"type":"number","minimum":-1,"maximum":1}},"level":{level},"facts":{{"type":"object","additionalProperties":{{"type":"string"}}}}"#
                ),
                r#""level""#,
            ),
            spec(
                "raise_level",
                format!(r#""memory_id":{{"type":"integer"}},"level":{level}"#),
                r#""memory_id","level""#,
            ),
        ]);
        let tools = &p[p.find("- remember").unwrap()..];
        assert_eq!(
            tools,
            "- remember: remember it.\n  level: \"faint\"|\"seared\" — How deep.\n  \
             facts?: {\"<key>\": string, ...}\n  valence?: number -1..1\n\
             - raise_level: raise_level it.\n  level: \"faint\"|\"seared\"\n  memory_id: integer"
        );
    }
}
