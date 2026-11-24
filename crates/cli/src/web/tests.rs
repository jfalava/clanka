use super::common::{self, TOKEN};
use super::{Console, router};
use clanka::setup::Paths;
use clanka_core::model::{Actor, Level, MemoryType, Speaker};
use clanka_core::{Companion, MemoryDraft, Mood};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

const RUN: &str = "this-runs-token";

/// A console over the one companion `common::open` keeps in `dir`, served on
/// a port of its own. Returns where it listens.
fn serve(dir: &Path) -> String {
    let all_in = |dir: &Path| Paths {
        config: dir.to_owned(),
        souls: dir.to_owned(),
        data: dir.to_owned(),
        models: dir.to_owned(),
    };
    let console = Console::new(all_in(dir), RUN.into(), |soul| {
        Ok(common::open(
            soul.parent().expect("a soul.md in a directory"),
        ))
    });
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

/// The status and body of a GET that carries this run's token.
fn get(base: &str, path: &str) -> (u16, Value) {
    let mut response = agent()
        .get(format!("{base}{path}"))
        .header("Authorization", format!("Bearer {RUN}"))
        .call()
        .unwrap();
    let body = response.body_mut().read_to_string().unwrap();
    (
        response.status().as_u16(),
        serde_json::from_str(&body).unwrap_or(Value::String(body)),
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
    c.supersede(Actor::Writer, opinion, &firmer).unwrap();
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
    let base = serve(dir.path());
    let companion = format!("/api/companions/{}", id(&c));

    let update = std::env::var_os("CLANKA_UPDATE_FIXTURES").is_some();
    let mut stale = Vec::new();
    for (name, path) in [
        ("companions", "/api/companions".to_owned()),
        ("overview", companion.clone()),
        ("memories", format!("{companion}/memories")),
        ("memory", format!("{companion}/memories/{fact}")),
        ("facts", format!("{companion}/facts")),
        ("recall", format!("{companion}/recall?q=Marisol")),
        ("log", format!("{companion}/log")),
    ] {
        let (status, mut answer) = get(&base, &path);
        assert_eq!(status, 200, "{path}: {answer}");
        settled(&mut answer, &id(&c), &dir.path().display().to_string());
        let sent = serde_json::to_string_pretty(&answer).unwrap() + "\n";
        let file = Path::new(FIXTURES).join(format!("{name}.json"));
        if update {
            std::fs::write(&file, sent).unwrap();
        } else if std::fs::read_to_string(&file).ok().as_deref() != Some(sent.as_str()) {
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "the server no longer sends what web/test/fixtures holds for {stale:?}; if that is \
         meant, run `CLANKA_UPDATE_FIXTURES=1 cargo test -p clanka --bin clanka web`, then \
         bring web/src/api.ts in line (`bun run web:check`) and rebuild the page"
    );
}
