mod common;

use clanka::turn::{self, Begin};
use clanka_core::model::{Actor, Level, MemoryType, Section};
use clanka_core::{Companion, MemoryDraft};
use common::{TOKEN, open};

fn begin(c: &mut Companion, message: &str, first: bool) -> (String, turn::Pending) {
    match turn::begin(c, message, 4000, first).unwrap() {
        Begin::Open { context, pending } => (context, pending),
        other => panic!("not open: {other:?}"),
    }
}

#[test]
fn the_doctor_token_freezes_memory_and_is_never_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());

    // Surrounding whitespace doesn't hide the token.
    assert_eq!(
        turn::begin(&mut c, &format!(" {TOKEN}\n"), 4000, true).unwrap(),
        Begin::Doctor
    );
    assert!(c.is_frozen().unwrap());
    assert_eq!(
        turn::begin(&mut c, "hello?", 4000, true).unwrap(),
        Begin::Frozen
    );

    // Neither message reached the transcript: the first turn after unfreezing is turn 1.
    c.set_frozen(false).unwrap();
    let (_, pending) = begin(&mut c, "hi", true);
    assert_eq!(pending.user_turn, 1);
}

/// A host that replays what it was given gets the whole context once, then
/// only what the model hasn't seen: a changed section, a new note, a memory not
/// yet recalled this session. A turn with nothing new is empty.
#[test]
fn later_turns_carry_only_what_is_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());
    c.write_section(Actor::Cli, Section::AboutUser, "Likes tea.")
        .unwrap();
    // The dummy embedder puts every text in one spot, so a second memory of the
    // same type would be refused as a near-duplicate.
    let draft = |kind, text: &str| MemoryDraft::new(kind, Level::Normal, text);
    c.write(
        Actor::Cli,
        &draft(MemoryType::UserFact, "The user drinks tea"),
    )
    .unwrap();

    let (first, pending) = begin(&mut c, "hi", true);
    assert!(first.contains("Likes tea."), "{first}");
    assert!(first.contains("The user drinks tea"), "{first}");
    let (user, _) = turn::end(&mut c, pending, "hello").unwrap();

    // Nothing changed since: nothing to say.
    let (second, pending) = begin(&mut c, "still here", false);
    assert_eq!(second, "");
    turn::end(&mut c, pending, "yes").unwrap();

    // The writer left a note and a new memory and rewrote a section.
    c.write_turn_note(Actor::Writer, user, "They seemed tired.")
        .unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::Preference, "The user has a cat"),
    )
    .unwrap();
    c.write_section(Actor::Writer, Section::Relationship, "Easy.")
        .unwrap();
    let (third, pending) = begin(&mut c, "anything?", false);
    assert!(third.contains("They seemed tired."), "{third}");
    assert!(third.contains("The user has a cat"), "{third}");
    assert!(third.contains("Easy."), "{third}");
    assert!(!third.contains("Likes tea."), "{third}");
    assert!(!third.contains("The user drinks tea"), "{third}");
    turn::end(&mut c, pending, "no").unwrap();

    // A new host conversation against the same open session starts over.
    let (again, _) = begin(&mut c, "hi again", true);
    assert!(again.contains("Likes tea."), "{again}");
    assert!(again.contains("The user drinks tea"), "{again}");
}

/// `>>N` at the start asks for a longer reply this turn only. It reaches the
/// model even on a turn with nothing new, and memory sees only the message.
#[test]
fn a_leading_stretch_marker_lifts_the_length_rule_for_one_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());
    let (_, pending) = begin(&mut c, "hi", true);
    turn::end(&mut c, pending, "hello").unwrap();

    let (quiet, pending) = begin(&mut c, " >>3 she ducks under the blade", false);
    assert_eq!(quiet, turn::stretch_hint(3));
    turn::end(&mut c, pending, "…").unwrap();
    let (spaced, pending) = begin(&mut c, ">> 2 she parries", false);
    assert_eq!(spaced, turn::stretch_hint(2));
    turn::end(&mut c, pending, "…").unwrap();

    // Mid-message, or glued to a word, it is just text.
    let (plain, pending) = begin(&mut c, "a >>3 b", false);
    assert_eq!(plain, "");
    turn::end(&mut c, pending, "…").unwrap();
    let (plain, pending) = begin(&mut c, ">>3rd try", false);
    assert_eq!(plain, "");
    turn::end(&mut c, pending, "…").unwrap();

    // Alone it means go on; past the cap it asks for the most there is.
    let (more, pending) = begin(&mut c, ">>40", false);
    assert_eq!(more, turn::stretch_hint(turn::MAX_STRETCH));
    turn::end(&mut c, pending, "…").unwrap();

    let said: Vec<String> = c
        .assemble("x", 4000, &clanka_core::Ranking::default())
        .unwrap()
        .turns
        .into_iter()
        .filter(|t| t.speaker == clanka_core::model::Speaker::User)
        .map(|t| t.content)
        .collect();
    assert_eq!(
        said,
        [
            "hi",
            "she ducks under the blade",
            "she parries",
            "a >>3 b",
            ">>3rd try",
            "Go on."
        ]
    );
}

/// `clanka chat` builds the prompt itself: it gets the whole context, with the
/// session's earlier turns but not the message it is about to send.
#[test]
fn an_opened_turn_carries_the_history_before_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());
    let first = match turn::open(&mut c, "hi", 4000, true).unwrap() {
        turn::Opened::Open(open) => *open,
        other => panic!("not open: {other:?}"),
    };
    assert!(first.context.turns.is_empty());
    assert!(first.context.render().contains("Nova"));
    assert_eq!(turn::end(&mut c, first.pending, "hello").unwrap(), (1, 2));

    let second = match turn::open(&mut c, ">>3 and then?", 4000, true).unwrap() {
        turn::Opened::Open(open) => *open,
        other => panic!("not open: {other:?}"),
    };
    let said: Vec<&str> = second
        .context
        .turns
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(said, ["hi", "hello"]);
    assert_eq!(
        (second.message.as_str(), second.stretch),
        ("and then?", Some(3))
    );
    assert_eq!(second.pending.user_turn, 3);
}

/// `~~` falls asleep into a dream, ending the conversation first; `<<` is
/// an echo only in a dream; text between `||` is out of earshot only awake.
/// Each marker reaches the model as this reply's hint, and memory never sees
/// the marker itself.
#[test]
fn dream_markers_start_a_dream_and_mark_echoes_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());

    // Closed, it is strikethrough; `~~~` is a code fence; `<<` awake is just
    // text.
    let (_, pending) = begin(&mut c, "~~not~~ a dream", true);
    assert!(!pending.dream);
    turn::end(&mut c, pending, "Ha.").unwrap();
    let (_, pending) = begin(&mut c, "~~~\ncode\n~~~", false);
    assert!(!pending.dream);
    turn::end(&mut c, pending, "Neat.").unwrap();
    let (said, pending) = begin(&mut c, "<< years ago ||she lied||", false);
    assert!(!pending.dream);
    assert!(said.ends_with(turn::UNHEARD_HINT), "{said}");
    turn::end(&mut c, pending, "Mm.").unwrap();

    // Mid-conversation, `~~` ends the conversation, which waits to be slept
    // on, and starts the dream; glued to the dream's first words it still does.
    let session = c.session_in_progress().unwrap().unwrap();
    let (first, pending) = begin(&mut c, "~~You're in a kitchen.", false);
    assert!(pending.dream && !pending.wake);
    assert_eq!(pending.closed, Some(session));
    assert!(pending.consolidates());
    assert_ne!(pending.session, session);
    assert_eq!(c.unconsolidated().unwrap(), [session]);
    assert!(first.contains("somewhere you know"), "{first}");
    assert!(!first.contains(turn::UNHEARD_HINT), "{first}");
    turn::end(&mut c, pending, "It smells of bread.").unwrap();

    // Closed by `>>`, `<<` is typed guillemets; glued to a scene, an echo.
    let (said, pending) = begin(&mut c, "<<Hola>>, she says.", false);
    assert!(!said.contains(clanka_core::dream::ECHO_HINT), "{said}");
    turn::end(&mut c, pending, "Hola.").unwrap();
    let (echo, pending) = begin(&mut c, "<<Rain on the window.", false);
    assert!(echo.contains(clanka_core::dream::ECHO_HINT), "{echo}");
    turn::end(&mut c, pending, "I remember.").unwrap();

    let (echo, pending) = begin(&mut c, "<< >>3 The judge reads the sentence.", false);
    assert!(echo.contains(clanka_core::dream::ECHO_HINT), "{echo}");
    assert!(echo.ends_with(&turn::stretch_hint(3)), "{echo}");
    assert!(!echo.contains("somewhere you know"), "said once: {echo}");
    turn::end(&mut c, pending, "No.").unwrap();

    let said: Vec<String> = c
        .assemble("x", 4000, &clanka_core::Ranking::default())
        .unwrap()
        .turns
        .into_iter()
        .map(|t| t.content)
        .collect();
    assert_eq!(
        said,
        [
            "You're in a kitchen.",
            "It smells of bread.",
            "<<Hola>>, she says.",
            "Hola.",
            "Rain on the window.",
            "I remember.",
            "The judge reads the sentence.",
            "No."
        ]
    );
}

/// `zz` at the start, in any case, says goodnight: awake, the companion sleeps
/// after this reply and is told so last; in a dream, the dream ends with it.
/// Memory keeps the goodnight without the mark, and a bare `zz` says
/// "Goodnight.". `zzz` or `zz` mid-message is just text.
#[test]
fn a_leading_zz_says_goodnight_and_wakes_a_dream() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());

    for plain in ["zzz", "so zz", "zzap"] {
        let (said, pending) = begin(&mut c, plain, false);
        assert!(
            !pending.sleep && !said.contains(turn::GOODNIGHT_HINT),
            "{plain}: {said}"
        );
        turn::end(&mut c, pending, "Mm.").unwrap();
    }
    let (said, pending) = begin(&mut c, " Zz night, love", false);
    assert!(pending.sleep && !pending.wake, "{pending:?}");
    assert!(said.ends_with(turn::GOODNIGHT_HINT), "{said}");
    let user_said = |c: &Companion| -> Vec<String> {
        c.assemble("x", 4000, &clanka_core::Ranking::default())
            .unwrap()
            .turns
            .into_iter()
            .filter(|t| t.speaker == clanka_core::model::Speaker::User)
            .map(|t| t.content)
            .collect()
    };
    assert_eq!(user_said(&c), ["zzz", "so zz", "zzap", "night, love"]);
    turn::end(&mut c, pending, "Sleep well.").unwrap();
    assert_eq!(c.session_in_progress().unwrap(), None, "the night ended");
    // A bare `zz` opens a new conversation and ends it at once.
    let (_, pending) = begin(&mut c, "zz", false);
    assert!(pending.sleep);
    assert_eq!(user_said(&c), [turn::GOODNIGHT]);
    turn::end(&mut c, pending, "Night.").unwrap();
    assert_eq!(c.unconsolidated().unwrap().len(), 2);

    // In a dream it wakes the companion instead of putting it to sleep.
    let (_, pending) = begin(&mut c, "~~ You're in a kitchen.", false);
    assert!(pending.dream && !pending.wake);
    turn::end(&mut c, pending, "It smells of bread.").unwrap();
    let (said, pending) = begin(&mut c, "zz", false);
    assert!(
        pending.dream && pending.wake && !pending.sleep,
        "{pending:?}"
    );
    assert!(said.contains(clanka_core::dream::WAKING_HINT), "{said}");
    assert!(said.ends_with(turn::WAKE_MARK_HINT), "{said}");
    assert!(!said.contains(turn::GOODNIGHT_HINT), "{said}");
}

/// A host keeps its handle open for days. After the persona is edited and
/// settled from another process (`clanka soul continue`), its next turn talks
/// under the settled soul instead of refusing it.
#[test]
fn an_open_handle_takes_up_a_persona_settled_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = open(dir.path());
    begin(&mut host, "hi", true);

    let soul = dir.path().join("soul.md");
    let edited = std::fs::read_to_string(&soul).unwrap() + "Now fond of tea.\n";
    std::fs::write(&soul, &edited).unwrap();
    open(dir.path()).continue_with_persona().unwrap();

    begin(&mut host, "still there?", false);
    assert!(host.soul().body.contains("Now fond of tea."));
}

/// A goodnight ends the night with its reply, before sleep runs: the next
/// message starts a new conversation at once, and the night waits for the
/// writer, which consolidates it whenever it gets to it.
#[test]
fn a_goodnight_ends_the_night_before_sleep_runs() {
    use clanka_core::deps::{LlmResponse, ToolCall};

    let dir = tempfile::tempdir().unwrap();
    let mut chat = open(dir.path());
    let (_, pending) = begin(&mut chat, "long day", true);
    turn::end(&mut chat, pending, "Tell me.").unwrap();
    let (_, pending) = begin(&mut chat, "zz night", false);
    let night = pending.session;
    assert!(pending.sleep && pending.consolidates());
    turn::end(&mut chat, pending, "Night.").unwrap();
    assert_eq!(chat.session_in_progress().unwrap(), None);

    // Before any sleep, the next message is a new conversation.
    let (_, next) = begin(&mut chat, "one more thing", false);
    assert_ne!(next.session, night);
    turn::end(&mut chat, next, "Go on.").unwrap();
    assert_eq!(chat.unconsolidated().unwrap(), [night]);

    let mut episode = Some(ToolCall {
        id: "e".into(),
        name: "write_episode".into(),
        arguments: r#"{"interpretation": "A long day, told.", "valence": 0.2}"#.into(),
    });
    let mut writer = common::open_with(
        dir.path(),
        common::Scripted::new(move |_| {
            Ok(LlmResponse {
                text: String::new(),
                tool_calls: episode.take().into_iter().collect(),
            })
        }),
    );
    let done = writer.consolidate().unwrap();
    assert_eq!(done.len(), 1);
    assert!(writer.unconsolidated().unwrap().is_empty());
    // The conversation after it is still going on.
    assert!(chat.session_in_progress().unwrap().is_some());
}
