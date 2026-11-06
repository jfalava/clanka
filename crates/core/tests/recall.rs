mod common;

use clanka_core::model::{Actor, Level, MemoryType, SessionEnd, Speaker};
use clanka_core::{Companion, Error, MemoryDraft, Mood, Ranking};
use common::{fixture, open, raw};
use serde_json::json;

fn write(c: &mut Companion, kind: MemoryType, level: Level, text: &str) -> i64 {
    c.write(Actor::Writer, &MemoryDraft::new(kind, level, text))
        .unwrap()
}

/// Pushes memories down by volume: rows without vectors, so they are never
/// recalled themselves and skip the duplicate check.
fn bury(c: &Companion, n: u32) {
    raw(c.db_path(), "k")
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO memories (type, level, interpretation, persona_hash, created_at, updated_at)
             SELECT 'episode', 'faint', 'filler ' || i, 'h', 0, 0 FROM n",
            [n],
        )
        .unwrap();
}

fn ids(c: &Companion, message: &str) -> Vec<i64> {
    c.recall(message, &Ranking::default())
        .unwrap()
        .into_iter()
        .map(|r| r.memory.id)
        .collect()
}

fn set_valence(c: &mut Companion, valence: f64) {
    let mood = Mood {
        valence,
        energy: 0.0,
        warmth: 0.0,
        cause: "test".into(),
    };
    c.set_mood(Actor::Writer, &mood).unwrap();
}

#[test]
fn burial_sinks_old_memories_and_recall_lifts_them_back() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let older = write(
        &mut c,
        MemoryType::UserFact,
        Level::Normal,
        "Laura's birthday is in May",
    );
    let newer = write(
        &mut c,
        MemoryType::Episode,
        Level::Normal,
        "Laura got a new job",
    );
    bury(&c, 400);
    assert_eq!(ids(&c, "Laura?"), [newer, older]);

    let session = c.open_session().unwrap();
    c.mark_recalled(session, &[older]).unwrap();
    assert_eq!(ids(&c, "Laura?"), [older, newer]);
    assert_eq!(c.memory(older).unwrap().recall_count, 1);
}

#[test]
fn levels_decide_what_survives_burial_and_seared_is_capped() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let kinds = [
        MemoryType::UserFact,
        MemoryType::Episode,
        MemoryType::Relationship,
    ];
    let seared: Vec<i64> = kinds
        .iter()
        .map(|&k| write(&mut c, k, Level::Seared, "The day Laura left"))
        .collect();
    let faint = write(&mut c, MemoryType::Promise, Level::Faint, "Call Laura back");
    let normal = write(
        &mut c,
        MemoryType::Preference,
        Level::Normal,
        "Laura likes jazz",
    );
    let vivid = write(
        &mut c,
        MemoryType::PersonaNote,
        Level::Vivid,
        "Laura scares me a bit",
    );
    let unrelated = write(&mut c, MemoryType::Episode, Level::Seared, "The concert");
    bury(&c, 3000);

    let got = ids(&c, "What about Laura?");
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(
        got[..2].iter().all(|id| seared.contains(id)),
        "seared first, at most two: {got:?}"
    );
    // Only vivid's floor keeps it in reach this deep; normal and faint have faded.
    assert_eq!(got[2], vivid);
    assert!(![faint, normal, unrelated].iter().any(|id| got.contains(id)));
}

#[test]
fn names_in_facts_count_and_odd_input_is_safe() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let mut marta = MemoryDraft::new(MemoryType::UserFact, Level::Normal, "Sam's dad is strict");
    let s = c.open_session().unwrap();
    marta.source_turns = vec![c.record_turn(s, Speaker::User, "My friend Marta").unwrap()];
    marta.facts.insert("friend".into(), json!("Marta"));
    let marta = c.write(Actor::Writer, &marta).unwrap();
    // Close it, so the recall window doesn't pull "Marta" into the query.
    c.end_session(s, SessionEnd::Sleep).unwrap();
    // Newer and just as close by meaning, but without the name.
    let other = write(
        &mut c,
        MemoryType::Episode,
        Level::Normal,
        "Dad cooks badly",
    );

    assert_eq!(ids(&c, "Dad?"), [other, marta]);
    assert_eq!(ids(&c, "Marta's dad?"), [marta, other]);
    for hostile in [r#"Marta" OR -NEAR(dad*"#, "\"", "", "a b", "*:^-"] {
        c.recall(hostile, &Ranking::default()).unwrap();
    }
}

#[test]
fn mood_pulls_same_feeling_memories_up() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let mut proud = MemoryDraft::new(
        MemoryType::UserFact,
        Level::Normal,
        "Laura was proud of Sam",
    );
    proud.valence = Some(0.8);
    let proud = c.write(Actor::Writer, &proud).unwrap();
    let mut fight = MemoryDraft::new(MemoryType::Episode, Level::Normal, "Laura yelled at Sam");
    fight.valence = Some(-0.8);
    let fight = c.write(Actor::Writer, &fight).unwrap();

    assert_eq!(ids(&c, "Laura"), [fight, proud], "no mood: newest first");
    set_valence(&mut c, 0.9);
    assert_eq!(ids(&c, "Laura"), [proud, fight]);
    set_valence(&mut c, -0.9);
    assert_eq!(ids(&c, "Laura"), [fight, proud]);
}

#[test]
fn the_query_covers_the_recall_window_of_user_messages() {
    let f = fixture(); // recall_window = 2
    let mut c = open(&f, "k").unwrap();
    let laura = write(
        &mut c,
        MemoryType::UserFact,
        Level::Normal,
        "Laura is Sam's sister",
    );
    write(
        &mut c,
        MemoryType::Episode,
        Level::Normal,
        "We went to a concert",
    );

    let s = c.open_session().unwrap();
    c.record_turn(s, Speaker::User, "I bought concert tickets")
        .unwrap();
    c.record_turn(s, Speaker::User, "Laura called").unwrap();
    c.record_turn(s, Speaker::Companion, "Oh? What did she say?")
        .unwrap();
    // The current message plus the one before it; the tickets are out of the window.
    assert_eq!(ids(&c, "Anyway"), [laura]);
}

#[test]
fn marking_recalls_respects_the_freeze() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let id = write(
        &mut c,
        MemoryType::UserFact,
        Level::Normal,
        "Laura is Sam's sister",
    );
    let s = c.open_session().unwrap();
    c.set_frozen(true).unwrap();
    assert_eq!(ids(&c, "Laura"), [id], "recall still works while frozen");
    assert!(matches!(c.mark_recalled(s, &[id]), Err(Error::Frozen)));
    assert_eq!(c.memory(id).unwrap().recall_count, 0);
}
