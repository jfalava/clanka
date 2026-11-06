mod common;

use clanka_core::model::{Actor, Section, Speaker};
use clanka_core::state::SECTION_MAX_CHARS;
use clanka_core::{Error, Mood};
use common::{count, fixture, open, raw};

fn mood(valence: f64, cause: &str) -> Mood {
    Mood {
        valence,
        energy: -1.0,
        warmth: 1.0,
        cause: cause.into(),
    }
}

#[test]
fn mood_keeps_history_and_reads_the_latest() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    assert_eq!(c.mood().unwrap(), None);

    c.set_mood(Actor::Writer, &mood(-0.4, "He blew me off"))
        .unwrap();
    let session = c.open_session().unwrap();
    c.set_mood(Actor::Writer, &mood(0.6, "He apologised"))
        .unwrap();
    assert_eq!(c.mood().unwrap(), Some(mood(0.6, "He apologised")));

    for bad in [mood(1.5, "Too happy"), mood(0.0, " ")] {
        let r = c.set_mood(Actor::Writer, &bad);
        assert!(matches!(r, Err(Error::Invalid(_))), "{bad:?}: {r:?}");
    }
    let db = raw(c.db_path(), "k");
    let sessions = "SELECT group_concat(coalesce(session_id, 'none')) FROM (SELECT session_id FROM mood_states ORDER BY id)";
    let seen: String = db.query_row(sessions, [], |r| r.get(0)).unwrap();
    assert_eq!(seen, format!("none,{session}"));
}

#[test]
fn the_last_turn_note_wins() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    assert_eq!(c.last_turn_note().unwrap(), None);
    let s = c.open_session().unwrap();
    let t1 = c
        .record_turn(s, Speaker::User, "Work is killing me")
        .unwrap();
    let t2 = c.record_turn(s, Speaker::Companion, "Then quit").unwrap();

    c.write_turn_note(Actor::Writer, t2, "Too blunt; check on him")
        .unwrap();
    c.write_turn_note(Actor::Writer, t1, "Still curious about the job")
        .unwrap();
    assert_eq!(
        c.last_turn_note().unwrap(),
        Some((t1, "Still curious about the job".into()))
    );
    assert!(matches!(
        c.write_turn_note(Actor::Writer, 999, "x"),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn sections_are_versioned_and_capped_in_characters() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    assert_eq!(c.section(Section::AboutYou).unwrap(), None);

    c.write_section(Actor::Writer, Section::AboutYou, "Lives in Madrid")
        .unwrap();
    c.write_section(Actor::Writer, Section::Relationship, "Early days")
        .unwrap();
    c.write_section(Actor::Writer, Section::AboutYou, "Moved to Lisbon")
        .unwrap();
    assert_eq!(
        c.section(Section::AboutYou).unwrap().as_deref(),
        Some("Moved to Lisbon")
    );
    assert_eq!(
        c.section(Section::Relationship).unwrap().as_deref(),
        Some("Early days")
    );
    let history: Vec<String> = c
        .section_history(Section::AboutYou)
        .unwrap()
        .into_iter()
        .map(|v| v.content)
        .collect();
    assert_eq!(history, ["Lives in Madrid", "Moved to Lisbon"]);

    // The cap counts characters, not bytes: "é" is two bytes.
    let full = "é".repeat(SECTION_MAX_CHARS);
    c.write_section(Actor::Writer, Section::Relationship, &full)
        .unwrap();
    let over = c.write_section(Actor::Writer, Section::Relationship, &format!("{full}!"));
    assert!(matches!(over, Err(Error::Invalid(_))), "{over:?}");
    assert_eq!(c.section(Section::Relationship).unwrap(), Some(full));
    assert_eq!(
        count(
            &raw(c.db_path(), "k"),
            "SELECT count(*) FROM soul_section_versions"
        ),
        4
    );
}
