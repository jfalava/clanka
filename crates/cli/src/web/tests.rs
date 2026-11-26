use super::common::{self, Scripted, TOKEN};
use super::{Console, Doctor, router};
use clanka::setup::Paths;
use clanka_core::deps::{LlmRequest, LlmResponse, ToolCall};
use clanka_core::diagnostics::{Event, Recorder};
use clanka_core::model::{Actor, Level, MemoryType, Speaker, Status};
use clanka_core::{Companion, MemoryDraft, Mood};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};

const RUN: &str = "this-runs-token";

fn all_in(dir: &Path) -> Paths {
    Paths {
        config: dir.to_owned(),
        souls: dir.to_owned(),
        data: dir.to_owned(),
        models: dir.to_owned(),
    }
}

fn beside(soul: &Path) -> &Path {
    soul.parent().expect("a soul.md in a directory")
}

/// A console over the one companion `common::open` keeps in `dir`, served on
/// a port of its own. Returns where it listens.
fn serve(dir: &Path) -> String {
    listen(Console::new(all_in(dir), RUN.into(), |soul| {
        Ok(common::open(beside(soul)))
    }))
}

/// [`serve`] for a run started with `--doctor`, with no `[model]`.
fn serve_doctor(dir: &Path) -> String {
    listen(
        Console::new(all_in(dir), RUN.into(), |soul| {
            Ok(common::open(beside(soul)))
        })
        .with_doctor(Doctor::new(None)),
    )
}

/// A `[model]` that re-reads every memory in `rewrites` in its first reply and
/// says nothing after.
fn rereader(rewrites: Vec<(i64, &'static str)>) -> (super::ModelName, super::Open) {
    let open = move |soul: &Path| {
        let rewrites = rewrites.clone();
        Ok(common::open_with(
            beside(soul),
            Scripted::new(move |request: &LlmRequest| {
                let tool_calls = if request.messages.len() == 1 {
                    rewrites
                        .iter()
                        .map(|(id, text)| ToolCall {
                            id: format!("re{id}"),
                            name: "reinterpret".into(),
                            arguments: json!({ "memory_id": id, "interpretation": text })
                                .to_string(),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(LlmResponse {
                    text: String::new(),
                    tool_calls,
                })
            }),
        ))
    };
    (Box::new(|_| Some("a-model".into())), Box::new(open))
}

fn listen(console: Console) -> String {
    let (port, listening) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            port.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, router(Arc::new(console)))
                .await
                .unwrap();
        });
    });
    format!("http://127.0.0.1:{}", listening.recv().unwrap())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

fn answered(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let body = response.body_mut().read_to_string().unwrap();
    (
        response.status().as_u16(),
        serde_json::from_str(&body).unwrap_or(Value::String(body)),
    )
}

/// The status and body of a GET that carries this run's token.
fn get(base: &str, path: &str) -> (u16, Value) {
    answered(
        agent()
            .get(format!("{base}{path}"))
            .header("Authorization", format!("Bearer {RUN}"))
            .call()
            .unwrap(),
    )
}

/// The status and body of a POST as the server's own page sends one: with
/// this run's token, from the server's own address.
fn post(base: &str, path: &str, body: Value) -> (u16, Value) {
    answered(
        agent()
            .post(format!("{base}{path}"))
            .header("Authorization", format!("Bearer {RUN}"))
            .header("Origin", base)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .unwrap(),
    )
}

fn id(c: &Companion) -> String {
    c.id().to_string()
}

/// An evening's worth: a fact with one value that was said and one that was
/// not, raised a level by hand, and an episode that cites nothing.
fn seed(c: &mut Companion) -> (i64, i64, i64) {
    let session = c.open_session().unwrap();
    let said = c
        .record_turn(session, Speaker::User, "My sister Marisol moved away")
        .unwrap();
    let mut fact = MemoryDraft::new(
        MemoryType::UserFact,
        Level::Normal,
        "Their sister Marisol moved to Oslo",
    );
    fact.facts.insert("sister".into(), json!("Marisol"));
    fact.facts.insert("city".into(), json!("Oslo"));
    fact.source_turns = vec![said];
    let fact = c.write(Actor::Writer, &fact).unwrap();
    let episode = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(
                MemoryType::Episode,
                Level::Faint,
                "We talked about the move",
            ),
        )
        .unwrap();
    c.set_level(Actor::Cli, fact, Level::Vivid).unwrap();
    (said, fact, episode)
}

#[test]
fn the_api_answers_only_this_runs_token_at_a_local_name() {
    let dir = tempfile::tempdir().unwrap();
    common::open(dir.path());
    let base = serve(dir.path());
    let ask = |token: Option<&str>, host: Option<&str>| {
        let mut request = agent().get(format!("{base}/api/companions"));
        if let Some(token) = token {
            request = request.header("Authorization", token);
        }
        if let Some(host) = host {
            request = request.header("Host", host);
        }
        request.call().unwrap().status().as_u16()
    };
    let bearer = format!("Bearer {RUN}");

    assert_eq!(ask(Some(&bearer), None), 200);
    // A tunnel may give the page another port; the name is what matters.
    assert_eq!(ask(Some(&bearer), Some("localhost:9000")), 200);
    assert_eq!(ask(Some(&bearer), Some("LOCALHOST")), 200);

    assert_eq!(ask(None, None), 401);
    assert_eq!(ask(Some(RUN), None), 401, "the token without Bearer");
    assert_eq!(ask(Some("Bearer this-runs-tokem"), None), 401);
    assert_eq!(ask(Some("Bearer this-runs-token-and-more"), None), 401);
    assert_eq!(ask(Some("Bearer "), None), 401);
    // The soul's doctor token is not this run's.
    assert_eq!(ask(Some(&format!("Bearer {TOKEN}")), None), 401);

    // Another site's name pointed at this machine, with the right token.
    assert_eq!(ask(Some(&bearer), Some("evil.example")), 403);
    assert_eq!(ask(Some(&bearer), Some("127.0.0.1.evil.example:80")), 403);
    assert_eq!(ask(Some(&bearer), Some("localhost.evil.example")), 403);
    // A Serve forward kept on this machine reaches a run without --tailnet.
    assert_eq!(ask(Some(&bearer), Some("box.example.ts.net:7333")), 403);

    // The page's files are no secret, but they answer to a local name only.
    let page = |host: Option<&str>| {
        let mut request = agent().get(format!("{base}/"));
        if let Some(host) = host {
            request = request.header("Host", host);
        }
        request.call().unwrap().status().as_u16()
    };
    assert!(matches!(page(None), 200 | 404), "{}", page(None));
    assert_eq!(page(Some("evil.example")), 403);
}

#[test]
fn with_tailnet_it_answers_at_the_names_serve_forwards_and_no_others() {
    let dir = tempfile::tempdir().unwrap();
    let c = common::open(dir.path());
    let base = listen(
        Console::new(all_in(dir.path()), RUN.into(), |soul| {
            Ok(common::open(beside(soul)))
        })
        .with_tailnet(vec!["box.example.ts.net".into()])
        .with_doctor(Doctor::new(None)),
    );
    let ask = |token: bool, host: &str| {
        let mut request = agent()
            .get(format!("{base}/api/companions"))
            .header("Host", host);
        if token {
            request = request.header("Authorization", format!("Bearer {RUN}"));
        }
        request.call().unwrap().status().as_u16()
    };

    assert_eq!(ask(true, "box.example.ts.net:7333"), 200);
    assert_eq!(ask(true, "box.example.ts.net"), 200);
    assert_eq!(ask(true, "BOX.Example.TS.net:443"), 200);
    assert_eq!(ask(true, "localhost:7333"), 200);
    assert_eq!(ask(false, "box.example.ts.net:7333"), 401);

    // Names that are not the one Serve forwards: another machine or another
    // tailnet on ts.net, the short name, and another site wearing it.
    assert_eq!(ask(true, "other.example.ts.net:7333"), 403);
    assert_eq!(ask(true, "box.other.ts.net:7333"), 403);
    assert_eq!(ask(true, "box:7333"), 403);
    assert_eq!(ask(true, "x.box.example.ts.net:7333"), 403);
    assert_eq!(ask(true, "box.example.ts.net.evil.example:7333"), 403);

    // Through Serve the page is HTTPS, so a change must name that page.
    let freeze = format!("{base}/api/companions/{}/freeze", id(&c));
    let change = |host: &str, origin: &str| {
        agent()
            .post(&freeze)
            .header("Host", host)
            .header("Authorization", format!("Bearer {RUN}"))
            .header("Origin", origin)
            .send_empty()
            .unwrap()
            .status()
            .as_u16()
    };
    let serve = "box.example.ts.net:7333";
    assert_eq!(change(serve, "http://box.example.ts.net:7333"), 403);
    assert_eq!(change(serve, &base), 403);
    assert_eq!(change(serve, "https://box.example.ts.net:8443"), 403);
    let port = base.rsplit_once(':').unwrap().1;
    // On this machine the page is still plain HTTP.
    assert_eq!(
        change(
            &format!("127.0.0.1:{port}"),
            &format!("https://127.0.0.1:{port}")
        ),
        403
    );
    assert!(!c.is_frozen().unwrap());
    assert_eq!(change(serve, "https://box.example.ts.net:7333"), 200);
    assert!(c.is_frozen().unwrap());
}

/// What `tailscale serve status --json` prints on a machine like oci-box: a
/// forward to a Unix socket, one to another app, one to clanka's port under
/// a path, and clanka's own going to `clanka`.
fn serve_status(clanka: &str) -> Value {
    json!({
        "TCP": {
            "443": { "HTTPS": true },
            "3773": { "HTTPS": true },
            "7333": { "HTTPS": true },
            "8085": { "HTTPS": true },
        },
        "Web": {
            "box.example.ts.net:443": { "Handlers": { "/": { "Proxy": "unix:/run/rex/serve.sock" } } },
            "box.example.ts.net:3773": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:3773" } } },
            "box.example.ts.net:8085": { "Handlers": { "/opds": { "Proxy": "http://127.0.0.1:7333" } } },
            "box.example.ts.net:7333": { "Handlers": { "/": { "Proxy": clanka } } },
        }
    })
}

#[test]
fn the_tailnet_names_are_the_ones_serve_forwards_to_this_port() {
    let found = super::forwards(&serve_status("http://127.0.0.1:7333"), 7333).unwrap();
    assert_eq!(
        found,
        vec![(
            "box.example.ts.net".to_string(),
            "https://box.example.ts.net:7333".to_string()
        )]
    );
    assert!(super::forwards(&serve_status("http://localhost:7333"), 7333).is_ok());

    // Nothing forwards to another port, nor to this one under a path.
    let none = super::forwards(&serve_status("http://127.0.0.1:7333"), 7334).unwrap_err();
    assert!(none.to_string().contains("--https=7334"), "{none}");
    assert!(super::forwards(&Value::Null, 7333).is_err());
    assert!(super::forwards(&serve_status("http://127.0.0.1:73330"), 7333).is_err());

    // Served on 443, the address needs no port.
    let mut on_443 = serve_status("http://127.0.0.1:9999");
    on_443["Web"]["box.example.ts.net:443"]["Handlers"]["/"]["Proxy"] =
        json!("http://127.0.0.1:7333");
    on_443["Web"]["box.example.ts.net:7333"]["Handlers"]["/"]["Proxy"] =
        json!("http://127.0.0.1:9999");
    assert_eq!(
        super::forwards(&on_443, 7333).unwrap()[0].1,
        "https://box.example.ts.net"
    );

    // A forward run in the foreground is under its session.
    let foreground =
        json!({ "Foreground": { "session-1": serve_status("http://127.0.0.1:7333") } });
    assert_eq!(super::forwards(&foreground, 7333).unwrap().len(), 1);

    // Funnel would put it on the internet; plain HTTP would send the token in the clear.
    let mut funnel = serve_status("http://127.0.0.1:7333");
    funnel["AllowFunnel"] = json!({ "box.example.ts.net:7333": true });
    let refused = super::forwards(&funnel, 7333).unwrap_err();
    assert!(refused.to_string().contains("Funnel"), "{refused}");
    let mut plain = serve_status("http://127.0.0.1:7333");
    plain["TCP"]["7333"] = json!({ "HTTP": true });
    let refused = super::forwards(&plain, 7333).unwrap_err();
    assert!(refused.to_string().contains("without HTTPS"), "{refused}");
}

#[test]
fn nothing_but_a_get_is_answered_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = common::open(dir.path());
    let (_, fact, _) = seed(&mut c);
    let base = serve(dir.path());
    let companion = format!("/api/companions/{}", id(&c));
    let before = (
        c.memories().unwrap(),
        c.writes_since(i64::MIN).unwrap(),
        c.is_frozen().unwrap(),
    );

    for path in [
        "/api/companions".to_owned(),
        companion.clone(),
        format!("{companion}/memories"),
        format!("{companion}/memories/{fact}"),
        format!("{companion}/facts"),
        format!("{companion}/recall?q=Marisol"),
        format!("{companion}/log"),
        // What a run with --doctor would answer.
        format!("{companion}/freeze"),
        format!("{companion}/end-session"),
        format!("{companion}/delete"),
        format!("{companion}/rollback"),
        format!("{companion}/reinterpret"),
        "/".to_owned(),
    ] {
        let url = format!("{base}{path}");
        let bearer = format!("Bearer {RUN}");
        let statuses = [
            agent()
                .post(&url)
                .header("Authorization", &bearer)
                .send_empty(),
            agent()
                .put(&url)
                .header("Authorization", &bearer)
                .send_empty(),
            agent()
                .patch(&url)
                .header("Authorization", &bearer)
                .send_empty(),
            agent().delete(&url).header("Authorization", &bearer).call(),
        ]
        .map(|response| response.unwrap().status().as_u16());
        assert_eq!(statuses, [405; 4], "{path}");
    }

    // Every read, then: the memory, its log and the freeze are as they were.
    // (Opening the companion is the only thing the server did to its database.)
    for path in ["", "/memories", "/facts", "/recall?q=Marisol", "/log"] {
        assert_eq!(get(&base, &format!("{companion}{path}")).0, 200, "{path}");
    }
    assert_eq!(get(&base, &format!("{companion}/memories/{fact}")).0, 200);
    // A POST as the page of a run with --doctor would send it, and that run's previews.
    for path in ["/freeze", "/end-session"] {
        assert_eq!(post(&base, &format!("{companion}{path}"), json!({})).0, 405);
    }
    let ids = json!({ "ids": [fact] });
    assert_eq!(post(&base, &format!("{companion}/delete"), ids).0, 405);
    assert_eq!(get(&base, &format!("{companion}/delete?ids={fact}")).0, 404);
    assert_eq!(get(&base, &format!("{companion}/rollback?to=0")).0, 404);
    assert_eq!(get(&base, &companion).1["can_repair"], false);
    let after = (
        c.memories().unwrap(),
        c.writes_since(i64::MIN).unwrap(),
        c.is_frozen().unwrap(),
    );
    assert_eq!(before, after);
}

#[test]
fn a_companion_reads_back_as_its_memory_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = common::open(dir.path());
    let (said, fact, episode) = seed(&mut c);
    let base = serve(dir.path());
    let companion = format!("/api/companions/{}", id(&c));

    let (status, listed) = get(&base, "/api/companions");
    assert_eq!(status, 200);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id(&c));
    assert_eq!(listed[0]["name"], "Nova");
    assert!(listed[0]["db_bytes"].as_u64().unwrap() > 0);
    assert_eq!(listed[0]["error"], Value::Null);

    let (_, overview) = get(&base, &companion);
    assert_eq!(overview["name"], "Nova");
    assert_eq!(overview["session"], "conversation");
    assert_eq!(
        (&overview["frozen"], &overview["mood"], &overview["note"]),
        (&json!(false), &Value::Null, &Value::Null)
    );
    assert_eq!(overview["sections"][0]["name"], "about_user");
    assert_eq!(overview["sections"][1]["name"], "relationship");

    // What another process does shows on the next request, not at a restart.
    c.set_mood(
        Actor::Writer,
        &Mood {
            valence: -0.5,
            energy: 0.25,
            warmth: 0.75,
            cause: "Marisol left".into(),
        },
    )
    .unwrap();
    c.set_frozen(true).unwrap();
    let (_, overview) = get(&base, &companion);
    assert_eq!(overview["frozen"], true);
    assert_eq!(
        overview["mood"],
        json!({ "valence": -0.5, "energy": 0.25, "warmth": 0.75, "cause": "Marisol left" })
    );
    c.set_frozen(false).unwrap();

    let (_, memories) = get(&base, &format!("{companion}/memories"));
    let memories = memories.as_array().unwrap();
    assert_eq!(memories.len(), 2);
    let m = &memories[0];
    assert_eq!(
        (&m["id"], &m["kind"], &m["level"], &m["status"]),
        (
            &json!(fact),
            &json!("user_fact"),
            &json!("vivid"),
            &json!("active")
        )
    );
    assert_eq!(m["interpretation"], "Their sister Marisol moved to Oslo");
    assert_eq!(m["facts"], json!({ "sister": "Marisol", "city": "Oslo" }));
    assert_eq!(m["source_turns"], json!([said]));
    assert_eq!(
        (
            &memories[1]["id"],
            &memories[1]["level"],
            &memories[1]["source_turns"]
        ),
        (&json!(episode), &json!("faint"), &json!([]))
    );

    let (_, detail) = get(&base, &format!("{companion}/memories/{fact}"));
    let reasons: Vec<_> = detail["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["reason"].as_str().unwrap(), v["level"].as_str().unwrap()))
        .collect();
    assert_eq!(reasons, [("written", "normal"), ("cli", "vivid")]);
    assert_eq!(detail["turns"].as_array().unwrap().len(), 1);
    assert_eq!(
        (&detail["turns"][0]["id"], &detail["turns"][0]["speaker"]),
        (&json!(said), &json!("user"))
    );
    assert_eq!(
        detail["turns"][0]["content"],
        "My sister Marisol moved away"
    );

    // Oslo was never said; Marisol was.
    let (_, issues) = get(&base, &format!("{companion}/facts"));
    assert_eq!(
        issues,
        json!([{ "memory": fact, "fact": ["city", "Oslo"] }])
    );

    let (_, found) = get(&base, &format!("{companion}/recall?q=where%20is%20Marisol"));
    let ids: Vec<_> = found["found"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["memory"]["id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&fact), "{found}");
    assert!(found["limit"].as_u64().unwrap() > 0);
    // Looking is not recalling.
    assert_eq!(c.memory(fact).unwrap().recall_count, 0);

    // The log from a point in time on: a write at that very millisecond is in.
    let log = c.writes_since(i64::MIN).unwrap();
    assert!(log.len() >= 4, "{log:?}");
    let (_, all) = get(&base, &format!("{companion}/log"));
    assert_eq!(all.as_array().unwrap().len(), log.len());
    let at = log[2].created_at;
    let from = |since: i64| {
        let (_, entries) = get(&base, &format!("{companion}/log?since={since}"));
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    let expected = |since: i64| {
        log.iter()
            .filter(|e| e.created_at >= since)
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(from(at), expected(at));
    assert!(from(at).contains(&log[2].id));
    assert_eq!(from(at + 1), expected(at + 1));
    assert!(!from(at + 1).contains(&log[2].id));
    assert_eq!(all[0]["actor"], log[0].actor.as_str());
}

#[test]
fn what_is_not_there_is_a_404_and_looking_makes_no_database() {
    let dir = tempfile::tempdir().unwrap();
    let c = common::open(dir.path());
    let base = serve(dir.path());

    // A second companion that was set up and never opened.
    let never = uuid::Uuid::now_v7();
    let soul = std::fs::read_to_string(dir.path().join("soul.md")).unwrap();
    std::fs::write(
        dir.path().join("never.md"),
        soul.replace(&id(&c), &never.to_string())
            .replace("Nova", "Vesper"),
    )
    .unwrap();
    // And a file that is no soul.
    std::fs::write(dir.path().join("broken.md"), "not a soul").unwrap();

    let (_, listed) = get(&base, "/api/companions");
    let by_file = |file: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["soul"].as_str().unwrap().ends_with(file))
            .unwrap_or_else(|| panic!("{file} is not in {listed}"))
            .clone()
    };
    assert_eq!(
        (
            &by_file("never.md")["name"],
            &by_file("never.md")["db_bytes"]
        ),
        (&json!("Vesper"), &Value::Null)
    );
    assert_eq!(by_file("broken.md")["id"], Value::Null);
    assert!(by_file("broken.md")["error"].is_string());

    let (status, why) = get(&base, &format!("/api/companions/{never}"));
    assert_eq!(status, 404);
    assert!(
        why["error"].as_str().unwrap().contains("never been opened"),
        "{why}"
    );
    assert_eq!(
        get(&base, &format!("/api/companions/{never}/memories")).0,
        404
    );
    assert!(!dir.path().join(format!("{never}.db")).exists());

    let nobody = uuid::Uuid::now_v7();
    assert_eq!(get(&base, &format!("/api/companions/{nobody}")).0, 404);
    assert_eq!(
        get(&base, &format!("/api/companions/{}/memories/4040", id(&c))).0,
        404
    );
    assert_eq!(get(&base, "/api/nothing").0, 404);
    assert_eq!(get(&base, "/api/companions/not-an-id").0, 400);
}

/// A command as diagnostics note it: its name, how it went and why not.
type Noted = (&'static str, &'static str, Option<&'static str>);

/// Diagnostics that keep each command noted.
#[derive(Clone, Default)]
struct Commands(Arc<Mutex<Vec<Noted>>>);
impl Recorder for Commands {
    fn record(&self, _: Option<uuid::Uuid>, event: &Event) {
        if let Event::Command {
            name,
            outcome,
            error,
            ..
        } = event
        {
            self.0.lock().unwrap().push((name, outcome, *error));
        }
    }
}

#[test]
fn a_change_is_taken_only_from_this_servers_own_page() {
    let dir = tempfile::tempdir().unwrap();
    let c = common::open(dir.path());
    let base = serve_doctor(dir.path());
    let freeze = format!("{base}/api/companions/{}/freeze", id(&c));
    let ask = |token: bool, origin: Option<&str>| {
        let mut request = agent().post(&freeze);
        if token {
            request = request.header("Authorization", format!("Bearer {RUN}"));
        }
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        request.send_empty().unwrap().status().as_u16()
    };
    let port = base.rsplit_once(':').unwrap().1;

    // Another site's page with the token in hand, or anything that names no page.
    assert_eq!(ask(true, Some("http://evil.example")), 403);
    assert_eq!(ask(true, Some(&format!("https://127.0.0.1:{port}"))), 403);
    assert_eq!(
        ask(true, Some(&format!("http://127.0.0.1:{port}.evil.example"))),
        403
    );
    assert_eq!(ask(true, Some("null")), 403);
    assert_eq!(ask(true, None), 403);
    // The page under another name for this machine than the one it was asked at.
    assert_eq!(ask(true, Some(&format!("http://localhost:{port}"))), 403);
    // The right page without this run's token.
    assert_eq!(ask(false, Some(&base)), 401);
    assert!(!c.is_frozen().unwrap());

    assert_eq!(ask(true, Some(&base)), 200);
    assert!(c.is_frozen().unwrap());
    // A read needs no page to name: the address bar sends none.
    assert_eq!(get(&base, &format!("/api/companions/{}", id(&c))).0, 200);
}

#[test]
fn the_page_repairs_as_the_doctor_commands_do() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = common::open(dir.path());
    let (said, fact, episode) = seed(&mut c);
    let commands = Commands::default();
    let noting = commands.clone();
    let base = listen(
        Console::new(all_in(dir.path()), RUN.into(), move |soul| {
            Ok(common::open_noting(
                beside(soul),
                common::Dummy,
                noting.clone(),
            ))
        })
        .with_doctor(Doctor::new(None)),
    );
    let companion = format!("/api/companions/{}", id(&c));
    let at = |path: &str| format!("{companion}{path}");
    let overview = || get(&base, &companion).1;
    assert_eq!(
        (&overview()["can_repair"], &overview()["model"]),
        (&json!(true), &Value::Null)
    );

    // Freeze and unfreeze, logged as the doctor's.
    assert_eq!(
        post(&base, &at("/freeze"), json!({})),
        (200, json!({ "frozen": true }))
    );
    assert!(c.is_frozen().unwrap());
    assert_eq!(overview()["frozen"], true);
    assert_eq!(
        post(&base, &at("/unfreeze"), json!({})),
        (200, json!({ "frozen": false }))
    );
    assert!(!c.is_frozen().unwrap());
    let last = c.writes_since(i64::MIN).unwrap().pop().unwrap();
    assert_eq!(
        (last.actor.as_str(), last.action.as_str()),
        ("doctor", "unfreeze")
    );

    // A rollback is refused until the freeze is on, in the core's own words.
    let log = c.writes_since(i64::MIN).unwrap();
    let raised = log.iter().find(|e| e.action == "set_level").unwrap();
    let plan_path = at(&format!("/rollback?to={}", raised.created_at));
    let (status, planned) = get(&base, &plan_path);
    assert_eq!(status, 200, "{planned}");
    assert_eq!(planned["seen"], c.write_log_head().unwrap());
    let order = |seen: &Value| json!({ "to": raised.created_at, "seen": seen });
    let (status, why) = post(&base, &at("/rollback"), order(&planned["seen"]));
    assert_eq!(status, 409, "{why}");
    assert!(
        why["error"].as_str().unwrap().contains("freeze first"),
        "{why}"
    );
    assert_eq!(c.memory(fact).unwrap().level, Level::Vivid);

    // Frozen, a plan the memory has moved on from is refused too.
    post(&base, &at("/freeze"), json!({}));
    let (status, why) = post(&base, &at("/rollback"), order(&planned["seen"]));
    assert_eq!(status, 409, "{why}");
    assert!(
        why["error"].as_str().unwrap().contains("written to after"),
        "{why}"
    );
    assert_eq!(c.memory(fact).unwrap().level, Level::Vivid);

    // The plan is what the rollback then answers and does: the level raised
    // at that time is back, and what was written before it stays.
    let (_, planned) = get(&base, &plan_path);
    assert_eq!(
        planned["plan"],
        json!({
            "removed": [], "restored": [fact], "reopened": [], "kept_seared": [],
            "state_rows": 0
        })
    );
    let (status, done) = post(&base, &at("/rollback"), order(&planned["seen"]));
    assert_eq!((status, &done), (200, &planned["plan"]));
    assert_eq!(c.memory(fact).unwrap().level, Level::Normal);
    assert!(c.memory(episode).is_ok());
    post(&base, &at("/unfreeze"), json!({}));

    // Ending the conversation: once, and then there is none to end.
    assert_eq!(overview()["session"], "conversation");
    assert_eq!(
        post(&base, &at("/end-session"), json!({})),
        (200, json!({ "ended": "conversation" }))
    );
    assert_eq!(c.session_in_progress().unwrap(), None);
    assert_eq!(overview()["session"], "none");
    assert_eq!(
        post(&base, &at("/end-session"), json!({})),
        (200, json!({ "ended": "none" }))
    );

    // A delete with a memory that is not there deletes nothing.
    let (status, _) = post(
        &base,
        &at("/delete"),
        json!({ "ids": [episode, fact, 4040] }),
    );
    assert_eq!(status, 404);
    assert_eq!(c.memories().unwrap().len(), 2);
    assert_eq!(get(&base, &at("/delete?ids=4040")).0, 404);
    assert_eq!(get(&base, &at("/delete?ids=one")).0, 409);

    // Its plan is what it then answers and does: the turn only the fact cited goes.
    let (status, planned) = get(&base, &at(&format!("/delete?ids={fact},{episode}")));
    assert_eq!(
        (status, &planned),
        (200, &json!({ "turns": [said], "restored": [] }))
    );
    assert_eq!(c.memories().unwrap().len(), 2, "a plan deletes nothing");
    assert_eq!(
        post(&base, &at("/delete"), json!({ "ids": [fact, episode] })),
        (200, planned)
    );
    assert!(c.memories().unwrap().is_empty());
    assert!(c.source_turns(fact).unwrap().is_empty());
    let last = c.writes_since(i64::MIN).unwrap().pop().unwrap();
    assert_eq!(
        (last.actor.as_str(), last.action.as_str()),
        ("cli", "delete")
    );

    // Each is noted as the command it stands for, with the kind of refusal.
    let noted = commands.0.lock().unwrap().clone();
    for expected in [
        ("doctor freeze", "ok", None),
        ("doctor unfreeze", "ok", None),
        ("doctor rollback", "error", Some("not_frozen")),
        ("doctor rollback", "ok", None),
        ("doctor end-session", "ok", None),
        ("doctor delete", "error", Some("not_found")),
        ("doctor delete", "ok", None),
    ] {
        assert!(
            noted.contains(&expected),
            "{expected:?} is not in {noted:?}"
        );
    }
}

#[test]
fn memories_are_reread_on_the_model_a_batch_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = common::open(dir.path());
    let (_, fact, episode) = seed(&mut c);
    let companion = format!("/api/companions/{}", id(&c));
    let reread = format!("{companion}/reinterpret");

    // Without a [model], the refusal says where to put one.
    let plain = serve_doctor(dir.path());
    let (status, why) = post(&plain, &reread, json!({ "ids": [fact] }));
    assert_eq!(status, 409, "{why}");
    assert!(why["error"].as_str().unwrap().contains("[model]"), "{why}");

    let base = listen(
        Console::new(all_in(dir.path()), RUN.into(), |soul| {
            Ok(common::open(beside(soul)))
        })
        .with_doctor(Doctor::new(Some(rereader(vec![(
            fact,
            "Marisol moved away, and they miss her",
        )])))),
    );
    assert_eq!(get(&base, &companion).1["model"], "a-model");

    let (status, why) = post(&base, &reread, json!({ "ids": [fact] }));
    assert_eq!(status, 409, "{why}");
    assert!(
        why["error"].as_str().unwrap().contains("freeze first"),
        "{why}"
    );
    c.set_frozen(true).unwrap();
    // One that cites no turns has nothing to re-read.
    assert_eq!(post(&base, &reread, json!({ "ids": [episode] })).0, 409);
    let many: Vec<i64> = (1..=21).collect();
    assert_eq!(post(&base, &reread, json!({ "ids": many })).0, 409);
    assert_eq!(post(&base, &reread, json!({ "ids": [] })).0, 409);
    assert_eq!(
        c.memory(fact).unwrap().interpretation,
        "Their sister Marisol moved to Oslo"
    );

    assert_eq!(
        post(&base, &reread, json!({ "ids": [fact] })),
        (
            200,
            json!({ "rewritten": 1, "model_calls": 1, "refused": [] })
        )
    );
    let m = c.memory(fact).unwrap();
    assert_eq!(
        (m.interpretation.as_str(), m.status),
        ("Marisol moved away, and they miss her", Status::Active)
    );
    // The reads' own handle, which has no model, sees it at once.
    let (_, detail) = get(&base, &format!("{companion}/memories/{fact}"));
    assert_eq!(
        detail["versions"].as_array().unwrap().last().unwrap()["reason"],
        "reinterpreted"
    );
}

/// Where the page's tests read what the server sends.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/test/fixtures");

/// What differs from run to run, made the same: the companion's ID, where its
/// soul.md is, the size of its database, every time and every score.
fn settled(value: &mut Value, id: &str, dir: &str) {
    match value {
        Value::Object(fields) => {
            for (name, field) in fields.iter_mut() {
                let is_time = name.ends_with("_at") && field.is_number();
                if is_time || name == "db_bytes" {
                    *field = json!(1);
                } else if name == "relevance" || name == "score" {
                    *field = json!(0.5);
                } else {
                    settled(field, id, dir);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| settled(item, id, dir)),
        Value::String(text) => {
            *text = text
                .replace(id, "00000000-0000-7000-8000-000000000000")
                .replace(dir, "/souls");
        }
        _ => {}
    }
}

/// The page (`web/`) is TypeScript and declares what it expects of every
/// answer. Its tests decode these files with those declarations, and this test
/// keeps the files what the server sends, so a field renamed here fails one of
/// the two. To take up a change made on purpose:
/// `CLANKA_UPDATE_FIXTURES=1 cargo test -p clanka --bin clanka web`.
#[test]
fn the_answers_are_the_ones_the_page_was_built_to_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = common::open(dir.path());
    let (said, fact, _) = seed(&mut c);
    // Every optional field once with something in it.
    let mut thread = MemoryDraft::new(
        MemoryType::OpenThread,
        Level::Normal,
        "I should ask how the move went",
    );
    thread.urgency = Some(clanka_core::model::Urgency::Hot);
    thread.valence = Some(-0.25);
    thread.source_turns = vec![said];
    let thread = c.write(Actor::Writer, &thread).unwrap();
    c.resolve_thread(Actor::Writer, thread).unwrap();
    let mut opinion = MemoryDraft::new(MemoryType::Opinion, Level::Normal, "Oslo is too cold");
    opinion.strength = Some(0.5);
    let opinion = c.write(Actor::Writer, &opinion).unwrap();
    let mut firmer = MemoryDraft::new(MemoryType::Opinion, Level::Vivid, "Oslo is far too cold");
    firmer.strength = Some(0.75);
    let firmer = c.supersede(Actor::Writer, opinion, &firmer).unwrap();
    c.set_mood(
        Actor::Writer,
        &Mood {
            valence: -0.5,
            energy: 0.25,
            warmth: 0.75,
            cause: "Marisol left".into(),
        },
    )
    .unwrap();
    c.write_turn_note(Actor::Writer, said, "Ask about Marisol")
        .unwrap();
    c.write_section(
        Actor::Writer,
        clanka_core::model::Section::AboutUser,
        "Has a sister, Marisol.",
    )
    .unwrap();
    // A run with --doctor and a [model], so the repairs' answers are here too.
    // The model re-reads the fact and one memory it was not asked about.
    let base = listen(
        Console::new(all_in(dir.path()), RUN.into(), |soul| {
            Ok(common::open(beside(soul)))
        })
        .with_doctor(Doctor::new(Some(rereader(vec![
            (fact, "Marisol moved away"),
            (thread, "Not mine to re-read"),
        ])))),
    );
    let companion = format!("/api/companions/{}", id(&c));

    let update = std::env::var_os("CLANKA_UPDATE_FIXTURES").is_some();
    let mut stale = Vec::new();
    let mut keep = |name: &'static str, (status, mut answer): (u16, Value)| {
        assert_eq!(status, 200, "{name}: {answer}");
        settled(&mut answer, &id(&c), &dir.path().display().to_string());
        let sent = serde_json::to_string_pretty(&answer).unwrap() + "\n";
        let file = Path::new(FIXTURES).join(format!("{name}.json"));
        if update {
            std::fs::write(&file, sent).unwrap();
        } else if std::fs::read_to_string(&file).ok().as_deref() != Some(sent.as_str()) {
            stale.push(name);
        }
    };
    for (name, path) in [
        ("companions", "/api/companions".to_owned()),
        ("overview", companion.clone()),
        ("memories", format!("{companion}/memories")),
        ("memory", format!("{companion}/memories/{fact}")),
        ("facts", format!("{companion}/facts")),
        ("recall", format!("{companion}/recall?q=Marisol")),
        ("log", format!("{companion}/log")),
    ] {
        keep(name, get(&base, &path));
    }
    // The repairs, in an order each can run in: the plan of a delete, then
    // frozen, a re-read, and a rollback to before the level was raised.
    let at = |path: &str| format!("{companion}{path}");
    keep(
        "deletion",
        get(&base, &at(&format!("/delete?ids={fact},{firmer}"))),
    );
    keep("frozen", post(&base, &at("/freeze"), json!({})));
    keep(
        "reread",
        post(&base, &at("/reinterpret"), json!({ "ids": [fact] })),
    );
    let raised = c
        .writes_since(i64::MIN)
        .unwrap()
        .into_iter()
        .find(|e| e.action == "set_level")
        .unwrap()
        .created_at;
    let planned = get(&base, &at(&format!("/rollback?to={raised}")));
    let seen = planned.1["seen"].clone();
    keep("rollback_plan", planned);
    keep(
        "rollback",
        post(
            &base,
            &at("/rollback"),
            json!({ "to": raised, "seen": seen }),
        ),
    );
    keep("ended", post(&base, &at("/end-session"), json!({})));
    assert!(
        stale.is_empty(),
        "the server no longer sends what web/test/fixtures holds for {stale:?}; if that is \
         meant, run `CLANKA_UPDATE_FIXTURES=1 cargo test -p clanka --bin clanka web`, then \
         bring web/src/api.ts in line (`bun run web:check`) and rebuild the page"
    );
}
