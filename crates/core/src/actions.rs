//! Tool calls for a model that only returns text, such as a host's plugin
//! model API with no function calling. The model answers with one JSON object
//! of actions. Each action becomes the same [`ToolCall`] a native tool call
//! would, and runs through the same guarded handlers.
//!
//! The reader is forgiving about the wrapping and strict about the contents.
//! Small open models put prose, a code fence or a `<think>` block around the
//! JSON, leave a trailing comma, break a string over a raw newline, or call
//! the fields `name` and `args`; none of that changes what was asked for, so
//! it all parses. An action for a tool that isn't offered, or a reply with no
//! JSON object in it, refuses the whole reply: a malformed reply runs nothing.

use crate::deps::{ToolCall, ToolSpec};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Most actions one reply may carry. More is a malformed reply, not a batch.
pub const MAX_ACTIONS: usize = 16;

#[derive(Deserialize)]
struct Reply {
    #[serde(alias = "tool_calls", alias = "calls")]
    actions: Vec<Action>,
}

#[derive(Deserialize)]
struct Action {
    #[serde(alias = "name")]
    tool: String,
    #[serde(
        default,
        alias = "args",
        alias = "parameters",
        alias = "input",
        deserialize_with = "arguments"
    )]
    arguments: Map<String, Value>,
    /// Whatever else is on the action. GLM and DeepSeek often skip the
    /// `arguments` wrapper and put the arguments here.
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// The arguments as an object, also when the model wrapped them in a string.
fn arguments<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Map<String, Value>, D::Error> {
    match Value::deserialize(d)? {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        Value::String(s) => match serde_json::from_str(&repair(&s)) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(serde::de::Error::custom(
                "arguments must be a JSON object, not a string",
            )),
        },
        _ => Err(serde::de::Error::custom("arguments must be a JSON object")),
    }
}

/// What the system prompt adds so the model knows the tools and the reply
/// format. The format comes last, where a small model weighs it most.
pub fn protocol(tools: &[ToolSpec]) -> String {
    let mut out = String::from(
        "You have no function-calling interface here; you use tools by replying with one JSON \
         object of actions, described at the end.\n\nTools, with their arguments as \
         `name: type`. A `?` marks an optional argument; a field described once means the \
         same wherever it appears.",
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
    out.push_str(FORMAT);
    out
}

/// The reply format, after the tools.
const FORMAT: &str = "\n\nReply with ONLY this JSON object, no words before or after it and no \
    code fence:\n\
    {\"actions\": [{\"tool\": \"<tool name>\", \"arguments\": {<arguments>}}, ...]}\n\
    Every action you want goes in this one reply; they run in order. You hear back only if \
    one fails, so you can fix it. Nothing to do: {\"actions\": []}";

/// A property's JSON Schema as a short type: `string`, `string ≤300 chars`, `"a"|"b"`,
/// `number -1..1`, `{"<key>": string, ...}`.
fn type_of(p: &Value) -> String {
    if let Some(e) = p["enum"].as_array() {
        return e.iter().map(Value::to_string).collect::<Vec<_>>().join("|");
    }
    let ty = p["type"].as_str().unwrap_or("any");
    match ty {
        "string" => match p["maxLength"].as_u64() {
            Some(n) => format!("string ≤{n} chars"),
            None => ty.into(),
        },
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
/// An empty reply is nothing to do, not a reply to correct.
pub fn parse(text: &str, tools: &[ToolSpec]) -> Result<Vec<ToolCall>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let text = without_thinking(text);
    let reply = find_reply(&text)?;
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
            let arguments = if a.arguments.is_empty() {
                a.rest
            } else {
                a.arguments
            };
            Ok(ToolCall {
                id: format!("action-{i}"),
                name: a.tool,
                arguments: Value::Object(arguments).to_string(),
            })
        })
        .collect()
}

/// The reply without its `<think>…</think>` blocks. Reasoning left on is
/// thinking out loud, not a reply. A block that never closes is kept: the
/// reply is more likely after it than inside it.
fn without_thinking(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        let Some(end) = rest[start..].find("</think>") else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + end + "</think>".len()..];
    }
    out.push_str(rest);
    out
}

/// The one JSON value in the text that is a reply: `{"actions": [...]}`, a
/// bare `[...]` of actions, or a single `{"tool": ...}`. Whatever surrounds it
/// (prose, a code fence) is ignored, and sloppy JSON is repaired first.
fn find_reply(text: &str) -> Result<Reply, String> {
    let mut first_error = None;
    for (i, c) in text.char_indices().filter(|(_, c)| matches!(c, '{' | '[')) {
        let tail = &text[i..];
        // Always repaired: a duplicate "tool" key is valid JSON that would
        // otherwise parse to the last action only.
        let Some(value) = first_value(&repair(tail)) else {
            continue;
        };
        let looks_like_actions = c == '{'
            || value
                .as_array()
                .is_some_and(|a| a.first().is_some_and(Value::is_object));
        let parsed = match value {
            Value::Array(actions) => {
                serde_json::from_value(Value::Array(actions)).map(|actions| Reply { actions })
            }
            Value::Object(map) if map.contains_key("tool") || map.contains_key("name") => {
                serde_json::from_value(Value::Object(map)).map(|a| Reply { actions: vec![a] })
            }
            other => serde_json::from_value(other),
        };
        match parsed {
            Ok(reply) => return Ok(reply),
            // A stray bracket in prose parses as something else; keep looking,
            // but if nothing fits, say what was wrong with the first candidate.
            Err(e) if looks_like_actions => first_error.get_or_insert(e.to_string()),
            Err(_) => continue,
        };
    }
    Err(match first_error {
        Some(e) => format!("the reply must be {{\"actions\": [...]}} ({e})"),
        None => "the reply has no JSON object in it".into(),
    })
}

/// The first JSON value at the start of the text, whatever follows it.
fn first_value(text: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()?
        .ok()
}

/// JSON with the slips small models make fixed: a trailing comma before `}`
/// or `]`, a raw newline or tab inside a string, and a closing bracket too
/// many (`}}}]}` where `}}]}` was meant), which would otherwise end the reply
/// after its first action and lose the rest.
fn repair(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    // Each open container, and for an object whether it already has a "tool" key.
    let mut open: Vec<(char, bool)> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                '\n' => {
                    out.push_str("\\n");
                    continue;
                }
                '\t' => {
                    out.push_str("\\t");
                    continue;
                }
                '\r' => continue,
                _ => {}
            }
            out.push(c);
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                // A second "tool" key in one object is a second action the model
                // forgot to close the first one before: split them, or JSON would
                // silently keep only the last.
                if let Some((_, seen)) = open.last_mut().filter(|_| is_key(chars.clone(), "tool")) {
                    if *seen {
                        let kept = out.trim_end().trim_end_matches(',').len();
                        out.truncate(kept);
                        out.push_str("}, {");
                    }
                    *seen = true;
                }
            }
            '{' | '[' => open.push((c, false)),
            '}' | ']' => {
                // A closer that doesn't close the innermost open container is dropped.
                let opener = if c == '}' { '{' } else { '[' };
                if open.last().map(|o| o.0) != Some(opener) {
                    continue;
                }
                open.pop();
            }
            ',' => {
                // A comma whose next token closes the container is dropped.
                let mut ahead = chars.clone();
                while ahead.peek().is_some_and(|n| n.is_whitespace()) {
                    ahead.next();
                }
                if matches!(ahead.peek(), Some('}' | ']')) {
                    continue;
                }
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// Whether the string that just opened is the object key `name`: its
/// characters, the closing quote, then a colon.
fn is_key(mut ahead: std::iter::Peekable<std::str::Chars<'_>>, name: &str) -> bool {
    if !name.chars().all(|n| ahead.next() == Some(n)) || ahead.next() != Some('"') {
        return false;
    }
    while ahead.peek().is_some_and(|n| n.is_whitespace()) {
        ahead.next();
    }
    ahead.peek() == Some(&':')
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

    /// What the first call came out as: `(tool, arguments)`.
    fn first(text: &str) -> (String, String) {
        let calls = parse(text, &tools()).unwrap_or_else(|e| panic!("{e}: {text}"));
        (calls[0].name.clone(), calls[0].arguments.clone())
    }

    #[test]
    fn the_wrapping_is_forgiven_and_the_contents_are_not() {
        let ok = r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "hi"}}]}"#;
        let note = ("leave_note".to_string(), r#"{"note":"hi"}"#.to_string());
        assert_eq!(first(ok), note);
        assert!(parse(r#"{"actions": []}"#, &tools()).unwrap().is_empty());
        assert!(parse("  \n", &tools()).unwrap().is_empty());

        // The shapes small open models actually send, each meaning the same call.
        for sloppy in [
            format!("```json\n{ok}\n```"),
            format!("Sure! Here is what I'll keep:\n{ok}\nLet me know if that works."),
            format!("<think>He mentioned Lisbon {{twice}}.</think>\n{ok}"),
            format!("<think>unterminated thinking that runs on {ok}"),
            r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "hi",},},]}"#.into(),
            "{\"actions\": [{\"tool\": \"leave_note\", \"arguments\": {\"note\": \"hi\"}}]}"
                .replace("\"hi\"", "\"hi\"\n"),
            r#"[{"tool": "leave_note", "arguments": {"note": "hi"}}]"#.into(),
            r#"{"tool": "leave_note", "arguments": {"note": "hi"}}"#.into(),
            r#"{"actions": [{"name": "leave_note", "args": {"note": "hi"}}]}"#.into(),
            r#"{"tool_calls": [{"tool": "leave_note", "parameters": {"note": "hi"}}]}"#.into(),
            r#"{"actions": [{"tool": "leave_note", "arguments": "{\"note\": \"hi\"}"}]}"#.into(),
            r#"{"reasoning": "a note", "actions": [{"tool": "leave_note", "arguments": {"note": "hi"}, "why": "x"}]}"#.into(),
            // GLM and DeepSeek: the arguments on the action, no wrapper.
            r#"{"actions": [{"tool": "leave_note", "note": "hi"}]}"#.into(),
            r#"{"actions": [{"tool": "leave_note", "arguments": {}, "note": "hi"}]}"#.into(),
            // GLM: one closing brace too many.
            r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "hi"}}}]}"#.into(),
        ] {
            assert_eq!(first(&sloppy), note, "{sloppy}");
        }
        // The brace too many came after the first of two actions; both survive.
        let two = parse(
            r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "a"}}}, {"tool": "leave_note", "arguments": {"note": "b"}}]}"#,
            &tools(),
        )
        .unwrap();
        assert_eq!(
            two.iter().map(|c| c.arguments.as_str()).collect::<Vec<_>>(),
            [r#"{"note":"a"}"#, r#"{"note":"b"}"#]
        );
        // GLM: two actions written into one object, the second "tool" key
        // overwriting the first. Both survive, with or without the wrapper.
        for mashed in [
            r#"{"actions": [{"tool": "leave_note", "arguments": {"note": "a"}, "tool": "leave_note", "arguments": {"note": "b"}}]}"#,
            r#"{"actions": [{"tool": "leave_note", "note": "a", "tool" : "leave_note", "note": "b"}]}"#,
        ] {
            let two = parse(mashed, &tools()).unwrap();
            assert_eq!(
                two.iter().map(|c| c.arguments.as_str()).collect::<Vec<_>>(),
                [r#"{"note":"a"}"#, r#"{"note":"b"}"#],
                "{mashed}"
            );
        }
        // A raw newline inside a string survives as a newline, not a broken string.
        assert_eq!(
            first("{\"actions\": [{\"tool\": \"leave_note\", \"arguments\": {\"note\": \"hi\nthere\"}}]}").1,
            r#"{"note":"hi\nthere"}"#
        );
        // No arguments at all is an empty object; the handler says what's missing.
        // An action that forgot its array is still one action.
        for bare in [
            r#"{"actions": [{"tool": "leave_note"}]}"#,
            r#"{"actions": {"tool": "leave_note"}}"#,
        ] {
            assert_eq!(first(bare), ("leave_note".to_string(), "{}".to_string()));
        }

        for bad in [
            "Sure, I'll remember that.",
            "<think>only thinking</think>",
            r#"{"actions": [{"tool": "leave_note", "arguments": "note=hi"}]}"#,
            r#"{"actions": [{"tool": 3, "arguments": {}}]}"#,
            r#"{"actions": [{"tool": "delete_all", "arguments": {}}]}"#,
            r#"{"note": "hi"}"#,
        ] {
            let err = parse(bad, &tools()).expect_err(bad);
            assert!(!err.is_empty());
        }
        assert!(
            parse(
                r#"{"actions": [{"tool": "delete_all", "arguments": {}}]}"#,
                &tools()
            )
            .unwrap_err()
            .contains("delete_all")
        );
        let many = format!(
            r#"{{"actions": [{}]}}"#,
            vec![r#"{"tool": "leave_note", "arguments": {}}"#; MAX_ACTIONS + 1].join(",")
        );
        assert!(parse(&many, &tools()).is_err());
    }

    #[test]
    fn repair_leaves_well_formed_json_alone() {
        let fine = r#"{"a": "x, }", "b": [1, 2], "c": "q\"]", "d": "\\"}"#;
        assert_eq!(repair(fine), fine);
        assert_eq!(repair(r#"{"a": [1, 2, ], }"#), r#"{"a": [1, 2 ] }"#);
        // "tool" as a nested key or as a value is not a second action.
        let nested =
            r#"{"tool": "remember", "arguments": {"facts": {"tool": "hammer"}, "note": "tool"}}"#;
        assert_eq!(repair(nested), nested);
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
                    r#""valence":{{"type":"number","minimum":-1,"maximum":1}},"level":{level},"facts":{{"type":"object","additionalProperties":{{"type":"string"}}}},"interpretation":{{"type":"string","maxLength":300}}"#
                ),
                r#""interpretation","level""#,
            ),
            spec(
                "raise_level",
                format!(r#""memory_id":{{"type":"integer"}},"level":{level}"#),
                r#""memory_id","level""#,
            ),
        ]);
        let tools = &p[p.find("- remember").unwrap()..p.find(FORMAT).unwrap()];
        assert_eq!(
            tools,
            "- remember: remember it.\n  interpretation: string ≤300 chars\n  \
             level: \"faint\"|\"seared\" — How deep.\n  \
             facts?: {\"<key>\": string, ...}\n  valence?: number -1..1\n\
             - raise_level: raise_level it.\n  level: \"faint\"|\"seared\"\n  memory_id: integer"
        );
        assert!(p.ends_with(FORMAT), "the format is the last thing read");
    }
}
