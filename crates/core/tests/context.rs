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
fn a_replaying_host_hears_each_step_of_tiredness_once() {
    let f = fixture(); // tired_at = 0.7, sleep_at = 0.9
    let mut c = open(&f, "k").unwrap();
    let (mut turns, _) = furnished(&mut c);
    // One more short exchange: the session the last reply saw (`before`) is
    // then most of the session but not all, so a budget can sit the two on
    // different sides of a step, or the same side.
    let s = c.session_in_progress().unwrap().unwrap();
    for (speaker, text) in [
        (Speaker::User, "ok, and then what did she say?"),
        (Speaker::Companion, "Nothing. That was the point."),
    ] {
        c.record_turn(s, speaker, text).unwrap();
        turns.push(text.into());
    }
    let session = turns.iter().map(|t| estimate_tokens(t)).sum::<usize>() as f64;
    let before = turns[..turns.len() - 2]
        .iter()
        .map(|t| estimate_tokens(t))
        .sum::<usize>() as f64;
    let fixed = assemble(&c, 100_000).tokens - session as usize;
    let kept = |session_budget: f64| {
        let mut ctx = assemble(&c, fixed + session_budget.round() as usize);
        let now = ctx.fatigue;
        c.keep_new(&mut ctx).unwrap();
        (now, ctx.fatigue)
    };

    // Tiredness began with this exchange: the hint is new, so it stays.
    let (now, after) = kept((before / 0.7 + session / 0.7) / 2.0);
    assert!(matches!(now, Fatigue::Tired(_)), "{now:?}");
    assert_eq!(after, now);
    // Already tired before this exchange: the host's history still says so.
    let (now, after) = kept((before / 0.7 + session / 0.9) / 2.0);
    assert!(matches!(now, Fatigue::Tired(_)), "{now:?}");
    assert_eq!(after, Fatigue::Rested);
    // Tired then, sleepy now: a new step, said once more.
    let (now, after) = kept((before / 0.9 + session / 0.9) / 2.0);
    assert_eq!((now, after), (Fatigue::Sleepy, Fatigue::Sleepy));
}

#[test]
fn a_changed_persona_blocks_assembly() {
    let f = fixture();
    open(&f, "k").unwrap();
    std::fs::write(&f.soul, soul_text(f.id, 0.7, &["Kindness over comfort"])).unwrap();
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

#[test]
fn a_soul_with_a_sense_of_time_knows_how_long_ago_it_kept_something() {
    use clanka_core::context::age;
    const DAY: i64 = 24 * 60 * 60 * 1000;
    let ago = |days: i64| age(days * DAY, 0);
    assert_eq!(age(DAY - 1, 0), None);
    assert_eq!(ago(1).as_deref(), Some("yesterday"));
    assert_eq!(ago(13).as_deref(), Some("13 days ago"));
    assert_eq!(ago(14).as_deref(), Some("2 weeks ago"));
    assert_eq!(ago(60).as_deref(), Some("8 weeks ago"));
    assert_eq!(ago(61).as_deref(), Some("2 months ago"));
    assert_eq!(ago(365).as_deref(), Some("12 months ago"));
    assert_eq!(ago(800).as_deref(), Some("2 years ago"));

    use clanka_core::context::date;
    assert_eq!(date(0), "1970-01-01");
    assert_eq!(date(-1), "1969-12-31");
    assert_eq!(date(1_709_164_800_000), "2024-02-29");
    assert_eq!(date(978_307_199_999), "2000-12-31");
    assert_eq!(date(1_794_268_800_000), "2026-11-10");

    let kept = |time_sense: bool| {
        let f = fixture();
        if !time_sense {
            let soul = std::fs::read_to_string(&f.soul).unwrap();
            assert!(soul.contains("time_sense = true"));
            std::fs::write(
                &f.soul,
                soul.replace("time_sense = true", "time_sense = false"),
            )
            .unwrap();
        }
        let clk = common::clock();
        let mut c = common::open_full(&f, |_| Default::default(), clk.clone());
        let draft = MemoryDraft::new(MemoryType::UserFact, Level::Normal, "Laura plays cello");
        c.write(Actor::Writer, &draft).unwrap();
        clk.fetch_add(21 * DAY, std::sync::atomic::Ordering::SeqCst);
        assemble(&c, 4000).render()
    };
    assert!(kept(true).contains("- (3 weeks ago) Laura plays cello"));
    assert!(kept(false).contains("- Laura plays cello"));

    // A memory written late, as a caught-up reflection is, dates from when it
    // was said, not from when it was written.
    let f = fixture();
    let clk = common::clock();
    let mut c = common::open_full(&f, |_| Default::default(), clk.clone());
    let s = c.open_session().unwrap();
    let said = c
        .record_turn(s, Speaker::User, "Laura plays cello")
        .unwrap();
    clk.fetch_add(5 * DAY, std::sync::atomic::Ordering::SeqCst);
    let mut draft = MemoryDraft::new(MemoryType::UserFact, Level::Normal, "Laura plays cello");
    draft.source_turns = vec![said];
    let id = c.write(Actor::Writer, &draft).unwrap();
    let memory = c.memory(id).unwrap();
    assert!(memory.created_at - memory.occurred_at.unwrap() >= 5 * DAY);
    assert!(
        assemble(&c, 4000)
            .render()
            .contains("- (5 days ago) Laura plays cello")
    );
}
