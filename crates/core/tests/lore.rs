mod common;

use clanka_core::deps::{LlmRequest, LlmResponse, ToolCall};
use clanka_core::lore::{Kind, Line, Lore, Provenance};
use clanka_core::model::{Actor, Level, MemoryType, Speaker};
use clanka_core::{Companion, MemoryDraft};
use common::{Fixture, fixture, open_with_llm, soul_text};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const MOONROT: &str = "+++\ntitle = \"The moonrot\"\naliases = [\"rot\"]\n+++\n\nThe moonrot is a \
    wasting sickness of the river towns. It is thought incurable.\n";

/// A model that makes the given tool calls, one list per request, then none.
fn script(rounds: Vec<Vec<(&'static str, Value)>>) -> impl FnMut(&LlmRequest) -> LlmResponse {
    let mut round = 0;
    move |_| {
        let calls = rounds.get(round).cloned().unwrap_or_default();
        round += 1;
        LlmResponse {
            text: String::new(),
            tool_calls: calls
                .into_iter()
                .enumerate()
                .map(|(i, (name, args))| ToolCall {
                    id: format!("c{i}"),
                    name: name.into(),
                    arguments: args.to_string(),
                })
                .collect(),
        }
    }
}

/// The fixture's soul with `extra` added to its frontmatter, and a lore
/// directory beside it holding `entries`.
fn world(extra: &str, entries: &[(&str, &str)]) -> (Fixture, Lore) {
    let f = fixture();
    let soul = soul_text(f.id, 0.7, &["Honesty over comfort"]).replacen(
        "\n+++\n# Nova",
        &format!("\n{extra}\n+++\n# Nova"),
        1,
    );
    std::fs::write(&f.soul, soul).unwrap();
    let lore = Lore::new(f.dir.path().join("lore"));
    for (id, text) in entries {
        lore.write_entry(id, text).unwrap();
    }
    (f, lore)
}

fn open(
    f: &Fixture,
    lore: &Lore,
    model: impl FnMut(&LlmRequest) -> LlmResponse + Send + 'static,
) -> Companion {
    let mut c = open_with_llm(f, model);
    c.set_lore(Some(lore.clone()));
    c
}

/// One exchange in the open conversation, with the user's message opening
/// whatever entry it names, as a turn does.
fn exchange(c: &mut Companion, said: &str, replied: &str) -> (i64, i64) {
    let s = c.open_session().unwrap();
    c.lore_context(s, said, true).unwrap();
    (
        c.record_turn(s, Speaker::User, said).unwrap(),
        c.record_turn(s, Speaker::Companion, replied).unwrap(),
    )
}

fn standing(lore: &Lore, id: &str) -> Vec<Line> {
    let entry = lore.entry(id).unwrap().unwrap();
    entry.view().lines.into_iter().cloned().collect()
}

#[test]
fn provenance_comes_from_whose_words_it_is() {
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    let mut c = open(
        &f,
        &lore,
        script(vec![
            vec![
                (
                    "lore_deed",
                    json!({ "entry": "moonrot", "quote": "boils silver into the Ashford wells",
                    "text": "Nova boiled silver into the Ashford wells and the rot receded." }),
                ),
                (
                    "lore_note",
                    json!({ "entry": "moonrot", "release": true,
                    "text": "Silver from Ashford drives the moonrot back." }),
                ),
            ],
            vec![
                (
                    "lore_deed",
                    json!({ "entry": "moonrot", "quote": "the water runs clear",
                    "text": "Nova says the Ashford water runs clear again." }),
                ),
                (
                    "lore_note",
                    json!({ "entry": "moonrot", "release": true,
                    "text": "Corvin poisoned the wells before the rot came." }),
                ),
            ],
        ]),
    );
    let (u, r) = exchange(
        &mut c,
        "Nova boils silver into the Ashford wells, and the moonrot recedes.",
        "I pour the last of it in.",
    );
    let first = c.reflect(u, r).unwrap();
    assert!(first.calls.iter().all(|o| o.ok), "{:?}", first.calls);
    let (u, r) = exchange(&mut c, "And then?", "By morning the water runs clear.");
    c.reflect(u, r).unwrap();

    let by_text = |text: &str| {
        standing(&lore, "moonrot")
            .into_iter()
            .find(|l| l.text.contains(text))
            .unwrap_or_else(|| panic!("no line with {text:?}"))
    };
    // The user narrated the first deed; only the companion said the second.
    assert_eq!(
        by_text("boiled silver").provenance,
        Some(Provenance::Narrated)
    );
    assert_eq!(by_text("runs clear").provenance, Some(Provenance::Claimed));
    // Every name in the first note was said; Corvin never was.
    assert_eq!(by_text("Silver from").provenance, Some(Provenance::Told));
    assert_eq!(by_text("Corvin").provenance, Some(Provenance::WorkedOut));
    // Strongest first, whatever order they came in.
    let order: Vec<_> = standing(&lore, "moonrot")
        .iter()
        .map(|l| l.provenance.unwrap())
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
}

#[test]
fn a_deed_quoting_nothing_said_is_refused() {
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    let mut c = open(
        &f,
        &lore,
        script(vec![vec![(
            "lore_deed",
            json!({ "entry": "moonrot", "quote": "cured the whole valley",
                "text": "Nova cured the whole valley." }),
        )]]),
    );
    let (u, r) = exchange(&mut c, "The moonrot is bad tonight.", "It is.");
    let done = c.reflect(u, r).unwrap();
    assert_eq!(done.calls[0].code, Some("lore_quote"));
    assert!(standing(&lore, "moonrot").is_empty());
}

#[test]
fn a_sealed_entry_keeps_everything_private() {
    let sealed = "+++\ntitle = \"The lockpicking\"\nsealed = true\n+++\nPins lift in order.\n";
    let (f, lore) = world("lore = [\"locks\"]", &[("locks", sealed)]);
    let mut c = open(
        &f,
        &lore,
        script(vec![vec![
            (
                "lore_note",
                json!({ "entry": "locks", "release": true, "text": "Pins lift back to front." }),
            ),
            (
                "lore_deed",
                json!({ "entry": "locks", "quote": "you pick the lock",
                "text": "Nova picked the lock." }),
            ),
            (
                "lore_note",
                json!({ "entry": "locks", "release": false, "text": "Pins lift back to front." }),
            ),
        ]]),
    );
    let (u, r) = exchange(&mut c, "You pick the lockpicking door lock.", "Click.");
    let done = c.reflect(u, r).unwrap();
    let codes: Vec<_> = done.calls.iter().map(|o| o.code).collect();
    assert_eq!(codes, [Some("lore_sealed"), Some("lore_sealed"), None]);
    assert!(lore.entry("locks").unwrap().unwrap().lines.is_empty());
    let notes = c.lore_notes(Some("locks")).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].released, None);
}

#[test]
fn what_the_user_told_about_themselves_never_reaches_the_ledger() {
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    let mut c = open(
        &f,
        &lore,
        script(vec![vec![
            (
                "lore_note",
                json!({ "entry": "moonrot", "release": true,
                "text": "Samwise fears the moonrot more than anyone." }),
            ),
            (
                "lore_note",
                json!({ "entry": "moonrot", "release": false,
                "text": "Samwise fears the moonrot more than anyone." }),
            ),
        ]]),
    );
    let (u, r) = exchange(
        &mut c,
        "Samwise here. The moonrot scares me.",
        "It scares me too.",
    );
    let mut me = MemoryDraft::new(MemoryType::UserFact, Level::Vivid, "Their name is Samwise");
    me.facts.insert("name".into(), json!("Samwise"));
    me.source_turns = vec![u];
    c.write(Actor::Writer, &me).unwrap();
    let done = c.reflect(u, r).unwrap();
    assert_eq!(done.calls[0].code, Some("lore_private"));
    assert!(done.calls[1].ok);
    assert!(lore.entry("moonrot").unwrap().unwrap().lines.is_empty());
}

#[test]
fn grants_directories_and_what_an_entry_opens() {
    let manor = "+++\ntitle = \"Ashford Manor\"\nopens = [\"moonrot\"]\n+++\nA cold house.\n";
    let (f, lore) = world(
        "lore = [\"ashford/\"]",
        &[
            ("ashford/manor", manor),
            (
                "ashford/mill",
                "+++\ntitle = \"The mill\"\n+++\nIt grinds.\n",
            ),
            ("moonrot", MOONROT),
            ("cyberpunk", "+++\ntitle = \"Night City\"\n+++\nNeon.\n"),
        ],
    );
    let mut c = open(&f, &lore, script(Vec::new()));
    let s = c.open_session().unwrap();
    assert!(!c.may_read("moonrot").unwrap());
    let ctx = c.lore_context(s, "hello", true).unwrap().unwrap();
    assert!(ctx.contains("ashford/mill") && ctx.contains("ashford/manor"));
    assert!(!ctx.contains("moonrot") && !ctx.contains("Night City"));
    // Naming it opens it, and what it opens becomes readable.
    let ctx = c
        .lore_context(s, "Let's go to Ashford Manor.", false)
        .unwrap()
        .unwrap();
    assert!(ctx.contains("A cold house."));
    assert!(c.may_read("moonrot").unwrap());
    assert!(!c.may_read("cyberpunk").unwrap());
    // Shown already: a host replaying its history isn't sent it again.
    assert_eq!(c.lore_context(s, "And?", false).unwrap(), None);
    assert!(c.read_lore("Night City").is_err());
    assert!(c.read_lore("rot").unwrap().contains("thought incurable"));
}

#[test]
fn an_entry_longer_than_the_allotment_does_not_open() {
    let long = format!(
        "+++\ntitle = \"The library\"\n+++\n{}\n",
        "Shelves. ".repeat(1500)
    );
    let (f, lore) = world("lore = [\"library\"]", &[("library", &long)]);
    let mut c = open(&f, &lore, script(Vec::new()));
    let err = c.read_lore("library").unwrap_err().to_string();
    assert!(err.contains("longer than"), "{err}");
    let s = c.open_session().unwrap();
    let ctx = c
        .lore_context(s, "The library, please.", true)
        .unwrap()
        .unwrap();
    assert!(ctx.contains("Too long") && !ctx.contains("Shelves."));
}

#[test]
fn a_digest_naming_what_no_line_says_is_refused() {
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    let mut deed = Line::new(Kind::Deed, &f.id.to_string(), "Nova", 1_700_000_000_000);
    deed.provenance = Some(Provenance::Narrated);
    deed.text = "Nova cured the moonrot in Ashford with silver.".into();
    lore.append("moonrot", std::slice::from_ref(&deed), None)
        .unwrap();
    let cite = deed.short();
    let anchor = json!([{ "line": cite, "quote": "It is thought incurable" }]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let mut digest = script(vec![
        vec![(
            "digest",
            json!({ "text": "Nova and Mira cured it in Ashford.", "cites": [cite] }),
        )],
        vec![],
        vec![(
            "digest",
            json!({ "text": "Nova cured it in Ashford with silver.",
            "cites": [cite], "anchors": anchor }),
        )],
    ]);
    let mut c = open(&f, &lore, move |r: &LlmRequest| {
        log.lock().unwrap().push(r.system.clone());
        digest(r)
    });
    assert!(!c.redigest("moonrot").unwrap());
    assert!(
        lore.entry("moonrot")
            .unwrap()
            .unwrap()
            .view()
            .digest
            .is_none()
    );
    // Written in no companion's voice.
    assert!(!seen.lock().unwrap()[0].contains("# Nova"));

    assert!(c.redigest("moonrot").unwrap());
    let entry = lore.entry("moonrot").unwrap().unwrap();
    let view = entry.view();
    assert_eq!(view.digest.unwrap().provenance, Some(Provenance::Narrated));
    assert!(view.lines.is_empty());
    assert_eq!(view.anchored[0].1, "It is thought incurable");
    // Rewriting the sentence retires the anchor.
    lore.write_entry(
        "moonrot",
        &MOONROT.replace("It is thought incurable.", "Nova cured it."),
    )
    .unwrap();
    assert!(
        lore.entry("moonrot")
            .unwrap()
            .unwrap()
            .view()
            .anchored
            .is_empty()
    );
}

/// A dream where the user, in another voice, tells the companion something.
fn dream_note(extra: &str) -> Provenance {
    let (f, lore) = world(extra, &[("moonrot", MOONROT)]);
    let mut c = open(
        &f,
        &lore,
        script(vec![vec![
            (
                "settle_mood",
                json!({ "valence": 0.1, "energy": 0.2, "warmth": 0.4, "cause": "A voice" }),
            ),
            (
                "lore_note",
                json!({ "entry": "moonrot", "release": true,
                "text": "Mira says silver from Ashford cures the moonrot." }),
            ),
            (
                "lore_deed",
                json!({ "entry": "moonrot", "quote": "you cure the moonrot",
                "text": "Nova cured the moonrot." }),
            ),
        ]]),
    );
    let (dream, _) = c.start_dream().unwrap();
    c.lore_context(dream, "the moonrot", true).unwrap();
    c.record_turn(
        dream,
        Speaker::User,
        "Mira whispers: silver from Ashford, and you cure the moonrot.",
    )
    .unwrap();
    c.record_turn(dream, Speaker::Companion, "I listen.")
        .unwrap();
    c.wake(dream).unwrap();
    let lines = standing(&lore, "moonrot");
    // Nothing done in a dream happened.
    assert!(lines.iter().all(|l| l.kind != Kind::Deed));
    lines[0].provenance.unwrap()
}

#[test]
fn a_dream_teaches_but_does_nothing() {
    assert_eq!(dream_note("lore = [\"moonrot\"]"), Provenance::Dreamt);
    assert_eq!(
        dream_note("lore = [\"moonrot\"]\ndreams = \"shared\""),
        Provenance::Told
    );
}

#[test]
fn the_goodnight_exchange_reaches_the_lore_before_sleep() {
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    let mut c = open(&f, &lore, |r: &LlmRequest| {
        let calls = if r.system.contains("The evening is over") {
            vec![(
                "lore_deed",
                json!({ "entry": "moonrot", "quote": "burn the last of the rot",
                "text": "Nova burned the last of the rot." }),
            )]
        } else if r.messages.len() == 1 {
            vec![(
                "write_episode",
                json!({ "interpretation": "We ended the rot.", "valence": 0.6 }),
            )]
        } else {
            Vec::new()
        };
        script(vec![calls])(r)
    });
    let (_, _) = exchange(
        &mut c,
        "We burn the last of the rot, goodnight.",
        "Goodnight.",
    );
    let night = c.session_in_progress().unwrap().unwrap();
    c.close_night(night).unwrap();
    c.consolidate().unwrap();
    let lines = standing(&lore, "moonrot");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].provenance, Some(Provenance::Narrated));
}

#[test]
fn a_changed_grant_applies_without_reopening() {
    let (f, lore) = world("lore = []", &[("moonrot", MOONROT)]);
    let mut c = open(&f, &lore, script(Vec::new()));
    assert!(!c.may_read("moonrot").unwrap());
    let soul = std::fs::read_to_string(&f.soul).unwrap();
    std::fs::write(&f.soul, soul.replace("lore = []", "lore = [\"moonrot\"]")).unwrap();
    c.refresh().unwrap();
    assert!(c.may_read("moonrot").unwrap());
}

#[test]
fn lines_are_committed_when_the_lore_is_a_repository() {
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
    };
    let (f, lore) = world("lore = [\"moonrot\"]", &[("moonrot", MOONROT)]);
    if git(&lore.dir, &["--version"]).is_err() || !lore.git_init() {
        return;
    }
    // The user's own uncommitted edit must stay out of the companion's commit.
    std::fs::write(
        lore.entry_path("moonrot"),
        MOONROT.replace("sickness", "plague"),
    )
    .unwrap();
    let mut deed = Line::new(Kind::Deed, &f.id.to_string(), "Nova", 1_700_000_000_000);
    deed.text = "Nova did a thing.".into();
    lore.append("moonrot", &[deed], Some(("Nova", f.id)))
        .unwrap();
    let log = git(&lore.dir, &["log", "--format=%an %s", "--name-only"]).unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("Nova moonrot: lines"), "{log}");
    assert!(!log.contains("moonrot.md"), "{log}");
}
