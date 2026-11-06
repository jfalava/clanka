mod common;

use clanka_core::model::Section;
use clanka_core::model::{Actor, Level, MemoryType, SessionEnd, Speaker, Status, Urgency};
use clanka_core::{Companion, Deleted, Error, MemoryDraft, Mood};
use common::{count, fixture, open, raw};
use rusqlite::Connection;
use serde_json::json;

fn draft(kind: MemoryType, text: &str) -> MemoryDraft {
    MemoryDraft::new(kind, Level::Normal, text)
}

fn fact(key: &str, value: &str, text: &str, turn: i64) -> MemoryDraft {
    let mut d = draft(MemoryType::UserFact, text);
    d.facts.insert(key.into(), json!(value));
    d.source_turns = vec![turn];
    d
}

fn with_turn(c: &mut Companion, text: &str) -> i64 {
    let session = c.open_session().unwrap();
    c.record_turn(session, Speaker::User, text).unwrap()
}

/// Rows in every table a memory write touches.
fn footprint(db: &Connection) -> Vec<i64> {
    [
        "SELECT count(*) FROM sessions",
        "SELECT count(*) FROM turns",
        "SELECT count(*) FROM memories",
        "SELECT count(*) FROM memory_versions",
        "SELECT count(*) FROM memory_sources",
        "SELECT count(*) FROM memory_vectors",
        "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'laura'",
        "SELECT count(*) FROM write_log WHERE action NOT IN ('freeze', 'unfreeze')",
    ]
    .iter()
    .map(|sql| count(db, sql))
    .collect()
}

#[test]
fn supersede_keeps_the_past_word_for_word() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let t1 = with_turn(&mut c, "I live in Madrid");
    let t2 = with_turn(&mut c, "I moved to Lisbon");
    let old = c
        .write(
            Actor::Writer,
            &fact("city", "Madrid", "Sam loves Madrid", t1),
        )
        .unwrap();
    let new = c
        .supersede(
            Actor::Writer,
            old,
            &fact("city", "Lisbon", "Sam left for Lisbon, maybe for Laura", t2),
        )
        .unwrap();

    let o = c.memory(old).unwrap();
    assert_eq!((o.status, o.superseded_by), (Status::Superseded, Some(new)));
    assert_eq!(o.interpretation, "Sam loves Madrid");
    assert_eq!(o.facts["city"], "Madrid");
    assert_eq!(o.source_turns, [t1]);
    let n = c.memory(new).unwrap();
    assert_eq!(
        (n.status, n.facts["city"].as_str()),
        (Status::Active, Some("Lisbon"))
    );
    assert_eq!(n.source_turns, [t2]);

    // The past can't be superseded twice, and a type can't change through it.
    let again = c.supersede(Actor::Writer, old, &fact("city", "Porto", "Porto now", t2));
    assert!(matches!(again, Err(Error::Invalid(_))), "{again:?}");
    let retyped = c.supersede(
        Actor::Writer,
        new,
        &draft(MemoryType::Episode, "Lisbon trip"),
    );
    assert!(matches!(retyped, Err(Error::Invalid(_))), "{retyped:?}");

    // A seared memory can become the past too, unchanged.
    let mut night = draft(MemoryType::Episode, "The night Sam told me about Sam's dad");
    night.level = Level::Seared;
    let seared = c.write(Actor::Writer, &night).unwrap();
    c.supersede(
        Actor::Writer,
        seared,
        &draft(MemoryType::Episode, "We finally talked it through"),
    )
    .unwrap();
    let s = c.memory(seared).unwrap();
    assert_eq!((s.status, s.level), (Status::Superseded, Level::Seared));
    assert_eq!(s.interpretation, "The night Sam told me about Sam's dad");
}

#[test]
fn freeze_from_another_handle_blocks_every_write_and_leaves_no_trace() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let turn = with_turn(&mut c, "My sister Laura called");
    let id = c
        .write(
            Actor::Writer,
            &fact("sister", "Laura", "Laura matters to Sam", turn),
        )
        .unwrap();
    let session = c.open_session().unwrap();
    let db = raw(c.db_path(), "k");
    let before = footprint(&db);

    open(&f, "k").unwrap().set_frozen(true).unwrap();
    let frozen = |r: clanka_core::Result<()>| assert!(matches!(r, Err(Error::Frozen)), "{r:?}");
    frozen(c.open_session().map(drop));
    frozen(c.record_turn(session, Speaker::User, "hello?").map(drop));
    frozen(
        c.write(
            Actor::Writer,
            &draft(MemoryType::Episode, "A concert night"),
        )
        .map(drop),
    );
    frozen(
        c.supersede(Actor::Writer, id, &fact("sister", "Lara", "Lara", turn))
            .map(drop),
    );
    frozen(c.set_level(Actor::Cli, id, Level::Vivid));
    let mood = Mood {
        valence: 0.2,
        energy: 0.1,
        warmth: 0.5,
        cause: "Laura called".into(),
    };
    frozen(c.set_mood(Actor::Writer, &mood));
    frozen(c.write_turn_note(Actor::Writer, turn, "Ask about Laura"));
    frozen(c.write_section(Actor::Writer, Section::AboutUser, "Has a sister, Laura"));
    assert_eq!(footprint(&db), before);
    let state = "SELECT (SELECT count(*) FROM mood_states) + (SELECT count(*) FROM turn_notes)
                        + (SELECT count(*) FROM soul_section_versions)";
    assert_eq!(count(&db, state), 0);

    // The doctor can still close the session while frozen.
    c.end_session(session, SessionEnd::Doctor).unwrap();
    c.set_frozen(false).unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::Episode, "A concert night"),
    )
    .unwrap();
}

#[test]
fn a_failed_write_leaves_nothing_behind() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let turn = with_turn(&mut c, "Laura is visiting");
    let db = raw(c.db_path(), "k");
    let before = footprint(&db);
    // The log entry is the last step of a write; failing it must undo the rest.
    db.execute_batch(
        "CREATE TRIGGER fail_log BEFORE INSERT ON write_log BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    )
    .unwrap();

    let err = c
        .write(
            Actor::Writer,
            &fact("sister", "Laura", "Laura is coming", turn),
        )
        .unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
    assert_eq!(footprint(&db), before);
}

#[test]
fn only_the_cli_can_lower_a_level_and_nobody_touches_seared() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let id = c
        .write(
            Actor::Writer,
            &draft(MemoryType::Episode, "We argued about the concert"),
        )
        .unwrap();

    let down = c.set_level(Actor::Writer, id, Level::Faint);
    assert!(
        matches!(
            down,
            Err(Error::LevelDown {
                current: Level::Normal,
                requested: Level::Faint
            })
        ),
        "{down:?}"
    );
    c.set_level(Actor::Writer, id, Level::Vivid).unwrap();
    c.set_level(Actor::Cli, id, Level::Faint).unwrap();
    assert_eq!(c.memory(id).unwrap().level, Level::Faint);
    let db = raw(c.db_path(), "k");
    let levels: Vec<String> = db
        .prepare(
            "SELECT level || ':' || reason FROM memory_versions WHERE memory_id = ?1 ORDER BY id",
        )
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(levels, ["normal:written", "vivid:escalated", "faint:cli"]);

    c.set_level(Actor::Writer, id, Level::Seared).unwrap();
    for actor in [Actor::Writer, Actor::Cli] {
        let r = c.set_level(actor, id, Level::Vivid);
        assert!(matches!(r, Err(Error::Seared(m)) if m == id), "{r:?}");
    }
}

#[test]
fn near_duplicates_are_refused_but_similar_memories_are_not() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let first = c
        .write(
            Actor::Writer,
            &draft(MemoryType::UserFact, "Sam misses Laura"),
        )
        .unwrap();

    let copy = c.write(
        Actor::Writer,
        &draft(MemoryType::UserFact, "Laura is always on Sam's mind"),
    );
    assert!(
        matches!(copy, Err(Error::Duplicate { existing, similarity }) if existing == first && similarity > 0.99),
        "{copy:?}"
    );
    // Same words under another type, or a related memory, are fine.
    c.write(
        Actor::Writer,
        &draft(MemoryType::Episode, "Sam misses Laura"),
    )
    .unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::UserFact, "Laura took Sam to a concert"),
    )
    .unwrap();
    // Superseding with near-identical text is the intended path.
    let new = c
        .supersede(
            Actor::Writer,
            first,
            &draft(MemoryType::UserFact, "Sam misses Laura a lot"),
        )
        .unwrap();
    // The superseded memory no longer blocks anything; the new one does.
    let again = c.write(Actor::Writer, &draft(MemoryType::UserFact, "Laura, again"));
    assert!(
        matches!(again, Err(Error::Duplicate { existing, .. }) if existing == new),
        "{again:?}"
    );
}

#[test]
fn writes_must_be_well_formed_and_cite_real_turns() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let turn = with_turn(&mut c, "I'm 34");

    let mut uncited = fact("age", "34", "Sam's 34", turn);
    uncited.source_turns.clear();
    let mut missing = fact("age", "34", "Sam's 34", turn);
    missing.source_turns = vec![turn, 999];
    let mut thread = draft(MemoryType::OpenThread, "Sam never answered about Sam's dad");
    let opinion = draft(MemoryType::Opinion, "Concerts are overrated");

    for bad in [&uncited, &missing, &thread, &opinion] {
        let r = c.write(Actor::Writer, bad);
        assert!(matches!(r, Err(Error::Invalid(_))), "{bad:?}: {r:?}");
    }
    thread.urgency = Some(Urgency::Hot);
    let id = c.write(Actor::Writer, &thread).unwrap();
    assert_eq!(c.memory(id).unwrap().urgency, Some(Urgency::Hot));
    assert_eq!(
        count(&raw(c.db_path(), "k"), "SELECT count(*) FROM memories"),
        1
    );
}

#[test]
fn sessions_are_shared_until_they_end() {
    let f = fixture();
    let mut a = open(&f, "k").unwrap();
    let mut b = open(&f, "k").unwrap();
    let s = a.open_session().unwrap();
    assert_eq!(b.open_session().unwrap(), s);

    b.end_session(s, SessionEnd::Sleep).unwrap();
    let r = a.record_turn(s, Speaker::Companion, "Goodnight");
    assert!(
        matches!(r, Err(Error::SessionClosed(id)) if id == s),
        "{r:?}"
    );
    assert!(matches!(
        a.end_session(s, SessionEnd::Sleep),
        Err(Error::SessionClosed(_))
    ));
    assert_ne!(a.open_session().unwrap(), s);
}

#[test]
fn delete_removes_everything_but_shared_turns_and_restores_the_past() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let t_madrid = with_turn(&mut c, "I live in Madrid");
    let t_shared = with_turn(&mut c, "Laura and my dad came to the concert");
    let t_own = with_turn(&mut c, "I moved to Lisbon for Laura");
    let t_free = with_turn(&mut c, "Anyway");

    let madrid = c
        .write(
            Actor::Writer,
            &fact("city", "Madrid", "Sam loves Madrid", t_madrid),
        )
        .unwrap();
    let mut lisbon = fact("city", "Lisbon", "Sam moved for Laura", t_own);
    lisbon.source_turns.push(t_shared);
    lisbon.level = Level::Seared;
    let lisbon = c.supersede(Actor::Writer, madrid, &lisbon).unwrap();
    let dad = c
        .write(
            Actor::Writer,
            &fact("dad", "came", "Sam's dad came to the concert", t_shared),
        )
        .unwrap();

    // Deleting is yours alone: it works on seared memories and during a freeze.
    c.set_frozen(true).unwrap();
    let deleted = c.delete_memory(lisbon).unwrap();
    assert_eq!(
        deleted,
        Deleted {
            turns: vec![t_own],
            restored: Some(madrid)
        }
    );
    assert!(matches!(c.memory(lisbon), Err(Error::NotFound(_))));
    assert!(matches!(c.delete_memory(lisbon), Err(Error::NotFound(_))));

    let m = c.memory(madrid).unwrap();
    assert_eq!((m.status, m.superseded_by), (Status::Active, None));
    assert_eq!(c.memory(dad).unwrap().source_turns, [t_shared]);

    let db = raw(c.db_path(), "k");
    let turns: Vec<i64> = db
        .prepare("SELECT id FROM turns ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(turns, [t_madrid, t_shared, t_free]);
    let gone = |sql: &str| {
        assert_eq!(
            count(&db, &sql.replace("?", &lisbon.to_string())),
            0,
            "{sql}"
        )
    };
    gone("SELECT count(*) FROM memory_versions WHERE memory_id = ?");
    gone("SELECT count(*) FROM memory_sources WHERE memory_id = ?");
    gone("SELECT count(*) FROM memory_vectors WHERE memory_id = ?");
    gone("SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'laura OR lisbon'");
    gone("SELECT count(*) FROM write_log WHERE payload LIKE '%Laura%' OR payload LIKE '%Lisbon%'");
    let nearest: i64 = count(
        &db,
        "SELECT memory_id FROM memory_vectors WHERE embedding MATCH '[1, 0, 0, 0.1]' AND k = 1",
    );
    assert_ne!(nearest, lisbon);
}
