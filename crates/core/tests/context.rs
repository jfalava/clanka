mod common;

use clanka_core::context::{estimate_tokens, hints_reserve};
use clanka_core::model::{Actor, Level, MemoryType, Section, Speaker, Urgency};
use clanka_core::{Companion, Context, Error, Fatigue, MemoryDraft, Mood, Ranking};
use common::{fixture, open, soul_text};

const MESSAGE: &str = "What about Laura?";

/// A companion with every part of the context filled in. Returns the session
/// turns in order and the hot thread's ID.
fn furnished(c: &mut Companion) -> (Vec<String>, i64) {
    c.write_section(Actor::Writer, Section::AboutUser, "Has a sister Sam adores")
        .unwrap();
    c.write_section(
        Actor::Writer,
        Section::Relationship,
        "Close, a little guarded",
    )
    .unwrap();
    let mood = Mood {
        valence: 0.3,
        energy: 0.1,
        warmth: 0.6,
        cause: "Sam came back early".into(),
    };
    c.set_mood(Actor::Writer, &mood).unwrap();

    let mut hot = MemoryDraft::new(
        MemoryType::OpenThread,
        Level::Normal,
        "Sam never said why Laura left",
    );
    hot.urgency = Some(Urgency::Hot);
    let hot = c.write(Actor::Writer, &hot).unwrap();
    let mut simmering = MemoryDraft::new(
        MemoryType::OpenThread,
        Level::Normal,
        "Who took the concert tickets?",
    );
    simmering.urgency = Some(Urgency::Simmering);
    c.write(Actor::Writer, &simmering).unwrap();
    for (kind, text) in [
        (MemoryType::UserFact, "Laura is Sam's older sister"),
        (MemoryType::Episode, "Laura and dad fought"),
        (MemoryType::Preference, "Laura sends Sam songs"),
    ] {
        c.write(Actor::Writer, &MemoryDraft::new(kind, Level::Normal, text))
            .unwrap();
    }

    let s = c.open_session().unwrap();
    let texts: Vec<String> = (0..6)
        .map(|i| format!("turn {i}: {}", "words ".repeat(5 + i * 3)))
        .collect();
    let mut last = 0;
    for (i, t) in texts.iter().enumerate() {
        let speaker = if i % 2 == 0 {
            Speaker::User
        } else {
            Speaker::Companion
        };
        last = c.record_turn(s, speaker, t).unwrap();
    }
    c.write_turn_note(Actor::Writer, last, "Ask about Laura gently")
        .unwrap();
    (texts, hot)
}

fn assemble(c: &Companion, budget: usize) -> Context {
    c.assemble(MESSAGE, budget, &Ranking::default()).unwrap()
}

#[test]
fn parts_come_in_the_designed_order() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let (turns, hot) = furnished(&mut c);
    let ctx = assemble(&c, 100_000);
    let text = ctx.render();

    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from:\n{text}"))
    };
    let order = [
        at("## Core values"),
        at("Has a sister Sam adores"),
        at("Close, a little guarded"),
        at("Sam came back early"),
        at("Ask about Laura gently"),
        at("Sam never said why Laura left"),
        at("Laura is Sam's older sister"),
    ];
    assert!(order.is_sorted(), "{order:?}\n{text}");
    // The hot thread is listed once, as a thread; the simmering one isn't a hot thread.
    assert_eq!(text.matches("Sam never said why Laura left").count(), 1);
    assert!(ctx.memories.iter().all(|r| r.memory.id != hot));
    assert_eq!(
        ctx.hot_threads.iter().map(|m| m.id).collect::<Vec<_>>(),
        [hot]
    );
    assert!(!text.contains("concert tickets"));

    let got: Vec<&str> = ctx.turns.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(got, turns);
    assert_eq!((ctx.omitted_turns, ctx.fatigue), (0, Fatigue::Rested));
}

#[test]
fn shrinking_the_budget_drops_the_least_important_first() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    furnished(&mut c);
    let full = assemble(&c, 100_000);
    let floor = estimate_tokens(&full.persona) + estimate_tokens(MESSAGE) + hints_reserve();

    let prefix = |part: &[i64], whole: &[i64]| whole.starts_with(part);
    let ids = |ctx: &Context| {
        (
            ctx.memories.iter().map(|r| r.memory.id).collect::<Vec<_>>(),
            ctx.turns.iter().map(|t| t.id).collect::<Vec<_>>(),
        )
    };
    let (all_memories, all_turns) = ids(&full);
    for budget in (0..=full.tokens).rev() {
        let ctx = assemble(&c, budget);
        assert_eq!(ctx.persona, full.persona, "the persona is never cut");
        if budget >= floor {
            assert!(
                ctx.tokens <= budget,
                "{} tokens over a budget of {budget}",
                ctx.tokens
            );
        }
        let (memories, turns) = ids(&ctx);
        assert!(
            prefix(&memories, &all_memories),
            "budget {budget}: memories {memories:?}"
        );
        assert!(
            all_turns.ends_with(&turns),
            "budget {budget}: kept {turns:?}, not the newest"
        );
        assert_eq!(ctx.omitted_turns, all_turns.len() - turns.len());
    }

    // Just short of everything: only the oldest turn goes.
    let ctx = assemble(&c, full.tokens - 1);
    assert_eq!(ctx.omitted_turns, 1);
    assert_eq!(ctx.memories.len(), full.memories.len());
}

#[test]
fn the_companion_tires_as_the_session_fills_its_share() {
    let f = fixture(); // tired_at = 0.7, sleep_at = 0.9
    let mut c = open(&f, "k").unwrap();
    let (turns, _) = furnished(&mut c);
    let session: usize = turns.iter().map(|t| estimate_tokens(t)).sum();
    let fixed = assemble(&c, 100_000).tokens - session;
    let at = |fill: f64| assemble(&c, fixed + (session as f64 / fill).round() as usize).fatigue;

    assert_eq!(at(0.6), Fatigue::Rested);
    match at(0.8) {
        Fatigue::Tired(level) => assert!((level - 0.5).abs() < 0.1, "{level}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(at(0.95), Fatigue::Sleepy);
    assert!(
        assemble(&c, fixed + session / 2)
            .render()
            .contains("Ask to sleep")
    );
}

#[test]
fn a_changed_persona_blocks_assembly() {
    let f = fixture();
    open(&f, "k").unwrap();
    std::fs::write(&f.soul, soul_text(f.id, 0.5, &["Honesty over comfort"])).unwrap();
    let c = open(&f, "k").unwrap();
    assert!(matches!(
        c.assemble(MESSAGE, 10_000, &Ranking::default()),
        Err(Error::PersonaUnresolved)
    ));
}

#[test]
fn the_companion_lets_go_at_its_pushback_limit_but_never_on_core_values() {
    let f = fixture(); // contrariness 0.6, pushback_window 20: 3 per 20 turns
    let mut c = open(&f, "k").unwrap();
    let s = c.open_session().unwrap();
    let mut turn = || c.record_turn(s, Speaker::Companion, "No.").unwrap();
    let turns: Vec<i64> = (0..6).map(|_| turn()).collect();
    let let_go = |c: &Companion| assemble(c, 10_000).let_go;

    for &t in &turns[..5] {
        c.record_pushback(Actor::Writer, t, true, "honesty")
            .unwrap();
    }
    c.record_pushback(Actor::Writer, turns[0], false, "Sam's job")
        .unwrap();
    c.record_pushback(Actor::Writer, turns[1], false, "Sam's music")
        .unwrap();
    assert_eq!(
        c.pushback_pressure().unwrap(),
        (2, 3),
        "core-value clashes don't count"
    );
    assert!(!let_go(&c));

    c.record_pushback(Actor::Writer, turns[2], false, "Sam's diet")
        .unwrap();
    assert!(let_go(&c));
    assert!(
        assemble(&c, 10_000)
            .render()
            .contains("Let the next disagreement go")
    );

    // Twenty turns later, those pushbacks have left the window.
    for _ in 0..16 {
        c.record_turn(s, Speaker::User, "ok").unwrap();
    }
    assert_eq!(
        c.pushback_pressure().unwrap(),
        (1, 3),
        "turns[2] is still the 20th from last"
    );
    c.record_turn(s, Speaker::User, "ok").unwrap();
    assert_eq!(c.pushback_pressure().unwrap(), (0, 3));
}
