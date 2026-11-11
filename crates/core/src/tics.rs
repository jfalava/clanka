//! Prose tics: the shapes a model's writing falls into and then repeats, in a
//! persona (where the companion picks them up) and in the companion's own
//! replies (where the last ones set the shape of the next).
//!
//! Only shapes a few words can pin down are looked for, so every finding is a
//! guess: "The shop isn't open. It's on the corner." reads the same as "That
//! isn't courage. It's the current." One such sentence is how people talk; a
//! tic is the shape coming back. Callers act on repetition, never on one hit.

/// A shape of sentence that models repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Denies one framing to reveal another: "That's not courage. That's the
    /// current."
    Negation,
    /// Calls something the real, whole or only one: "That's the whole trade."
    Label,
    /// Three or more clipped fragments in a row: "Few words. Hard ones. No
    /// speeches."
    Fragments,
    /// Ends on a general truth with nobody in it: "The river takes the rest."
    Maxim,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Negation, Kind::Label, Kind::Fragments, Kind::Maxim];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Negation => "negation-first reveal",
            Kind::Label => "significance label",
            Kind::Fragments => "clipped fragments",
            Kind::Maxim => "closing maxim",
        }
    }

    /// What to write instead, as an instruction.
    pub fn instead(self) -> &'static str {
        match self {
            Kind::Negation => "Say what it is without first saying what it is not",
            Kind::Label => "Say the thing without calling it the real, whole or only one",
            Kind::Fragments => "Write whole sentences",
            Kind::Maxim => "End on something said to the person, not a general truth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tic {
    pub kind: Kind,
    /// The words that carry it.
    pub quote: String,
}

/// The tics in running prose: a persona's body or a reply.
pub fn in_prose(text: &str) -> Vec<Tic> {
    let mut found = Vec::new();
    for block in blocks(text) {
        let words: Vec<_> = block.iter().map(|s| words(s)).collect();
        for (i, (sentence, w)) in block.iter().zip(&words).enumerate() {
            let next = words.get(i + 1);
            if next.is_some_and(|next| turns_on_next(w, next)) {
                found.push(Tic {
                    kind: Kind::Negation,
                    quote: format!("{} {}", shown(sentence), shown(&block[i + 1])),
                });
            } else if turns_within(w) {
                found.push(Tic {
                    kind: Kind::Negation,
                    quote: shown(sentence),
                });
            }
            if designates(w) && !asks(sentence) {
                found.push(Tic {
                    kind: Kind::Label,
                    quote: shown(sentence),
                });
            }
        }
        let mut run = 0;
        for (i, (sentence, w)) in block.iter().zip(&words).enumerate() {
            run = if is_fragment(sentence, w) { run + 1 } else { 0 };
            let ends = block
                .get(i + 1)
                .is_none_or(|next| !is_fragment(next, &words[i + 1]));
            if run >= 3 && ends {
                let run: Vec<_> = block[i + 1 - run..=i].iter().map(|s| shown(s)).collect();
                found.push(Tic {
                    kind: Kind::Fragments,
                    quote: run.join(" "),
                });
            }
        }
    }
    found
}

/// The maxim a persona's voice line ends on, if it does: its last sentence,
/// when another comes before it. Not looked for in
/// replies, where half of what it finds is small talk ("The bread is getting
/// cold.").
pub fn closing_maxim(text: &str) -> Option<Tic> {
    let all = sentences(text.trim());
    let last = all.last()?;
    (all.len() >= 2 && is_maxim(last)).then(|| Tic {
        kind: Kind::Maxim,
        quote: shown(last),
    })
}

/// A sentence as quoted back: without a "Good news:" label or the quote
/// marks of the voice line it is in.
fn shown(s: &str) -> String {
    let s = [": \"", ": \u{201c}"]
        .iter()
        .find_map(|open| s.rsplit_once(open))
        .map_or(s, |(_, quote)| quote);
    s.trim_matches(['"', '\u{201c}', '\u{201d}', '*', '_'])
        .to_owned()
}

/// The text as runs of prose: a paragraph or a list item each, split into
/// sentences. Headings and fenced code are left out.
fn blocks(text: &str) -> Vec<Vec<String>> {
    let mut blocks: Vec<String> = Vec::new();
    let mut open = false;
    let mut fenced = false;
    for line in text.lines().map(str::trim) {
        let fence = line.starts_with("```");
        fenced ^= fence;
        if fence || fenced || line.is_empty() || line.starts_with('#') {
            open = false;
            continue;
        }
        match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            Some(item) => blocks.push(item.to_owned()),
            None if open => {
                let last = blocks.last_mut().expect("an open block was pushed");
                last.push(' ');
                last.push_str(line);
            }
            None => blocks.push(line.to_owned()),
        }
        open = true;
    }
    blocks.iter().map(|b| sentences(b)).collect()
}

/// A run of prose split after each `.`, `!` or `?`, with any quote or
/// bracket that closes it, that a space or the end follows.
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if !matches!(c, '.' | '!' | '?' | '\u{2026}') {
            continue;
        }
        while chars
            .next_if(|&(_, c)| matches!(c, '"' | '\u{201d}' | ')' | '*' | '_'))
            .is_some()
        {}
        let end = match chars.peek() {
            None => text.len(),
            Some(&(at, c)) if c.is_whitespace() => at,
            Some(_) => continue,
        };
        let sentence = text[start..end].trim();
        if !sentence.is_empty() {
            out.push(sentence.to_owned());
        }
        start = end;
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        out.push(rest.to_owned());
    }
    out
}

/// A word in lower case with a straight apostrophe and no punctuation around
/// it, and whether a comma, colon, semicolon or dash closes the clause after
/// it.
type Word = (String, bool);

fn words(sentence: &str) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    // A dash glued between two words ("loved—they") still closes a clause.
    let spaced = sentence
        .replace("--", "\u{2014}")
        .replace('\u{2014}', " \u{2014} ");
    for raw in spaced.split_whitespace() {
        let raw = raw.replace('\u{2019}', "'");
        let breaks = raw.ends_with([',', ';', ':', '\u{2014}', '\u{2013}']) || raw == "-";
        let word = raw
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        match out.last_mut() {
            Some(last) if word.is_empty() => last.1 |= breaks,
            _ if word.is_empty() => {}
            _ => out.push((word, breaks)),
        }
    }
    out
}

fn any(list: &str, word: &str) -> bool {
    list.split(' ').any(|w| w == word)
}

const BE: &str = "is are was were am";
/// A subject and its "be" in one word.
const BE_JOINED: &str = "it's that's this's they're he's she's i'm you're we're";
const NOT_JOINED: &str = "isn't aren't wasn't weren't";
/// Something other than the two people talking.
const IT: &str = "it that this they he she";
const IT_IS: &str = "it's that's they're he's she's";
/// The two people talking. What one of them is not ("I'm not sure", "you're
/// not wrong") is a plain answer, not a reframing.
const PERSON: &str = "i you we i'm you're we're i'd you'd i'll you'll i've you've let's";

/// Where a sentence says what something is not: the index of its "is not",
/// "isn't" or "that's not". "Don't" and "never" alone deny an act, which is
/// no tic; nor is what one of the two talking is not ("I'm not sure", "you
/// are not wrong"), or a condition ("when it's not raining").
fn denial(w: &[Word]) -> Option<usize> {
    const CONDITIONS: &str = "if when unless while until once whether";
    let at = |i: usize| w[i].0.as_str();
    w.iter().enumerate().position(|(i, (s, _))| {
        // The index of what is denied something: the word before "isn't" or
        // "is not", or the "that's" of "that's not".
        let whose = if any(NOT_JOINED, s) {
            i.checked_sub(1)
        } else if s == "not" && i > 0 && any(BE_JOINED, at(i - 1)) {
            Some(i - 1)
        } else if s == "not" && i > 0 && any(BE, at(i - 1)) {
            // "The people around you is not a small thing" is not about you.
            i.checked_sub(2).filter(|_| at(i - 1) != "is")
        } else {
            return false;
        };
        let clause = (0..i).rev().find(|&j| j == 0 || w[j - 1].1).unwrap_or(0);
        !whose.is_some_and(|j| any(PERSON, at(j))) && !any(CONDITIONS, at(clause))
    })
}

/// Whether a clause opens by saying what something is: "It is", "That's",
/// "They are", with no denial of its own, about something other than the two
/// people talking. "It's okay" after "It's not your fault" reassures and
/// reframes nothing.
fn reveals(w: &[Word]) -> bool {
    const FINE: &str = "okay ok fine alright";
    let is = match w {
        [(a, _), rest @ ..] if any(IT_IS, a) => Some(rest),
        [(a, _), (b, _), rest @ ..] if any(IT, a) && any(BE, b) => Some(rest),
        _ => None,
    };
    is.is_some_and(|rest| !rest.first().is_some_and(|(s, _)| any(FINE, s)))
        && denial(&w[..w.len().min(3)]).is_none()
}

/// The verb a sentence denies doing ("does not give", "don't stay"), when
/// that is what it says.
fn denied_act(w: &[Word]) -> Option<&str> {
    w.windows(3).find_map(|p| {
        let (a, b, c) = (p[0].0.as_str(), p[1].0.as_str(), p[2].0.as_str());
        if matches!(a, "don't" | "doesn't" | "didn't") {
            Some(b)
        } else if matches!(a, "do" | "does" | "did") && b == "not" {
            Some(c)
        } else {
            None
        }
    })
}

/// Whether a clause opens by doing what was just denied: "It gives a fact"
/// after "It does not give a reason".
fn does_instead(w: &[Word], verb: &str) -> bool {
    let Some(((subject, _), rest)) = w.split_first() else {
        return false;
    };
    let same = |s: &str| {
        s == verb
            || ["s", "es", "d", "ed"]
                .iter()
                .any(|end| s.strip_suffix(end) == Some(verb))
    };
    any(IT, subject) && rest.iter().take(2).any(|(s, _)| same(s) && s.len() > 2)
}

/// "That is not superstition. That is a promise.", "Not the result. The part
/// that kept going.", "It does not give a reason. It gives a fact.": the
/// denial in one sentence and the reveal in the next.
fn turns_on_next(w: &[Word], next: &[Word]) -> bool {
    let opens_on_not = || {
        // "Not yet.", "Not really." and "Not stupid." answer someone; a noun
        // or a reason after "Not" sets up what comes next.
        const SETS_UP: &str = "a an the as like because for from";
        // What comes next is the thing itself ("The part that kept going.")
        // or takes up the same words ("Not as news. As a statement of
        // account."). "Not a problem. Happy to help." is an answer and a new
        // thought.
        const THING: &str = "a an the it that this they it's that's they're";
        matches!(w, [(not, _), (then, _), ..] if not == "not" && any(SETS_UP, then))
            && w.len() <= 8
            && next
                .first()
                .is_some_and(|(s, _)| any(THING, s) || w[1..].iter().any(|(said, _)| said == s))
    };
    let reveal_then_denial = || {
        // "That is the comfortable answer. It is not the true one."
        // "It's okay. It's not your fault." reassures; the reveal names a
        // thing ("the comfortable answer") for the denial to take back.
        let names = w.iter().take(3).any(|(s, _)| any("a an the", s));
        let again = next
            .first()
            .is_some_and(|(s, _)| any(IT, s) || any(IT_IS, s));
        reveals(w) && names && again && next.len() <= 8 && denial(next).is_some_and(|at| at <= 2)
    };
    denial(w).is_some() && reveals(next)
        || denied_act(w).is_some_and(|verb| does_instead(next, verb))
        || opens_on_not()
        || reveal_then_denial()
}

/// "It's not X, it's Y", "is not X, but Y", "is X, not Y" and "doesn't X; it
/// Ys" in one sentence.
fn turns_within(w: &[Word]) -> bool {
    // After a clause break: the index of the clause that follows it.
    let clauses = |from: usize| {
        w.iter()
            .enumerate()
            .skip(from)
            .filter(|(_, (_, breaks))| *breaks)
            .map(|(i, _)| i + 1)
    };
    // X, not Y: a clause that says what something is closes on a short
    // denial after a comma. "Once a conversation, not in every reply" has no
    // "is"; "You're a nurse, not a doctor" is a fact about one of the two
    // talking.
    const NO_NOUN: &str = "that to even just really yet for because if when until since with in \
        on at sure now always only so much quite";
    let tail = w.iter().enumerate().skip(1).find(|(i, (s, _))| {
        s == "not" && w[i - 1].1 && w.get(i + 1).is_some_and(|(s, _)| !any(NO_NOUN, s))
    });
    if let Some((not, _)) = tail {
        let start = (0..not)
            .rev()
            .find(|&j| j == 0 || w[j - 1].1 && j < not)
            .unwrap_or(0);
        let clause = &w[start..not];
        // "Work's a crossing" says what work is; "Anna's car" does not.
        let says = clause.iter().enumerate().any(|(j, (s, _))| {
            let article = clause.get(j + 1).is_some_and(|(s, _)| any("a an the", s));
            any(BE, s) || any(IT_IS, s) || s.ends_with("'s") && article
        });
        // The denial is a few words and the clause ends on it.
        let end = (not..w.len()).find(|&j| w[j].1).unwrap_or(w.len() - 1);
        if says && !any(PERSON, &clause[0].0) && denial(clause).is_none() && end - not <= 5 {
            return true;
        }
    }
    // Not X, but Y; isn't X, it's Y: a clause break soon after the denial,
    // then "but" and a noun, or a reveal.
    if let Some(not) = denial(w)
        && clauses(not).take_while(|&i| i <= not + 9).any(|i| {
            matches!(&w[i..], [(but, _), (then, _), ..]
                if but == "but" && !any(IT, then) && !any(PERSON, then) && !any(IT_IS, then) && then != "there")
                || reveals(&w[i..])
        })
    {
        return true;
    }
    // Doesn't X; it Ys.
    denied_act(w).is_some_and(|verb| clauses(0).any(|i| does_instead(&w[i..], verb)))
}

/// "Is the only honest thing", "that's the whole trade", "the real question
/// is", "is the part worth catching": calling something the real, whole or
/// only one, or the part that matters.
fn designates(w: &[Word]) -> bool {
    const LABELS: &str = "whole only real actual first one key true";
    const THINGS: &str = "thing things part bit point question answer trade trick difference \
        lesson problem reason job work mechanism sentence decision distinction";
    let is = |s: &str| matches!(s, "is" | "are" | "was") || any(BE_JOINED, s) || s.ends_with("'s");
    let at = |i: usize| w.get(i).map_or("", |(s, _)| s.as_str());
    (0..w.len()).any(|i| {
        // `i - 1` below is read only once `said` has found a word there.
        if at(i) != "the" {
            return false;
        }
        let said = i > 0 && is(at(i - 1));
        let (label, after) = (at(i + 1), at(i + 2));
        let named = any(THINGS, after) || any(THINGS, at(i + 3));
        // "Is the only honest thing", "that's the whole trade".
        said && any(LABELS, label) && named
            // "That's the point", "is the part worth catching".
            || said && any(THINGS, label) && (at(i - 1) == "that's" || matches!(after, "that" | "worth"))
            // "The real question is", "the distinction that matters".
            || matches!(label, "real" | "actual") && any(THINGS, after)
            || any(THINGS, label) && after == "that" && at(i + 3) == "matters"
    })
}

/// Whether a sentence is a clipped fragment: three words or fewer, stated,
/// with nobody named in it. "You live alone." and "Tell me." are short
/// sentences; "Hard ones." is a fragment, and so, wrongly, is "Sleep well."
fn is_fragment(sentence: &str, w: &[Word]) -> bool {
    const SUBJECTS: &str = "i you we he she it they that this there i'm you're we're it's that's \
        he's she's they're i'd i'll i've you'd you'll you've";
    // "Well..." trails off and "1." numbers a list.
    let end = sentence.trim_end_matches(['"', '\u{201d}', '*', '_', ')']);
    let stated = end.ends_with('.') && !end.ends_with("..");
    let number = w.iter().all(|(s, _)| s.chars().all(|c| c.is_ascii_digit()));
    stated
        && !number
        && (1..=3).contains(&w.len())
        && !any(SUBJECTS, &w[0].0)
        && !w.iter().any(|(s, _)| matches!(s.as_str(), "me" | "us"))
}

/// Whether a sentence is a question.
fn asks(sentence: &str) -> bool {
    sentence
        .trim_end_matches(['"', '\u{201d}', '*', '_', ')'])
        .ends_with('?')
}

/// Whether a sentence is a general truth: a statement whose subject is "the",
/// "most", "bad" or the like, with nobody in it to talk to. "That sounds
/// hard" and "Go on" are said to someone, so they are not.
pub fn is_maxim(sentence: &str) -> bool {
    const PERSONS: &str = "i i'm i'd i'll i've me my mine myself you you're you'd you'll you've \
        your yours yourself we we're us our let's";
    const GENERIC: &str = "the a an every most some nobody nothing everyone everything people \
        all bad good old";
    let w = words(sentence);
    w.len() >= 3
        && !asks(sentence)
        && any(GENERIC, &w[0].0)
        && !w.iter().any(|(s, _)| any(PERSONS, s))
}

/// Takes the em dashes out of a reply as it is written. A model that writes
/// one writes one in almost every reply (Kestrel in 13 of 13, Mira in 35 of
/// 37) whatever its persona says about them, and its own replies teach it the
/// next, so they go before the reply is shown or recorded. Pieces go in as
/// they arrive and come out with the dashes gone; spaces and dashes are held
/// back until what follows them is known, so call [`Dashless::finish`] last.
#[derive(Debug, Default)]
pub struct Dashless {
    /// The last character written on this line that is not a space.
    last: Option<char>,
    /// Spaces and dashes not yet written.
    held: String,
}

impl Dashless {
    pub fn push(&mut self, piece: &str) -> String {
        let mut out = String::with_capacity(piece.len());
        for c in piece.chars() {
            if c == ' ' || "\u{2013}\u{2014}\u{2015}".contains(c) {
                self.held.push(c);
                continue;
            }
            self.release(Some(c), &mut out);
            out.push(c);
            self.last = (c != '\n').then_some(c);
        }
        out
    }

    /// What was held back at the end of the reply.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.release(None, &mut out);
        out
    }

    fn release(&mut self, next: Option<char>, out: &mut String) {
        let held = std::mem::take(&mut self.held);
        // An en dash stands for an em dash only with a space beside it:
        // "1914–1918" is a range.
        let dash = held.contains(['\u{2014}', '\u{2015}'])
            || (held.contains('\u{2013}') && held.contains(' '));
        if !dash {
            out.push_str(&held);
            return;
        }
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        out.push_str(match (self.last, next) {
            // A line that opens on a dash: dialogue, or a name under a quote.
            (None, _) => "",
            (_, _) if digit(self.last) && digit(next) && !held.contains(' ') => "-",
            (Some(last), _) if "([\"\u{201c}\u{2018}*_".contains(last) => "",
            // Speech that breaks off: "I would never—"
            (Some(last), None | Some('\n' | '"' | '\u{201d}' | '*' | '_' | ')')) => {
                if ".,;:!?\u{2026}".contains(last) {
                    ""
                } else {
                    "..."
                }
            }
            (Some(last), _) if ".,;:!?\u{2026}".contains(last) => " ",
            (_, Some(next)) if ".,;:!?".contains(next) => "",
            _ => ", ",
        });
    }
}

/// `text` without its em dashes: each becomes a comma, and one that breaks a
/// sentence off becomes "...".
pub fn without_dashes(text: &str) -> String {
    let mut dashless = Dashless::default();
    let mut out = dashless.push(text);
    out.push_str(&dashless.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Kind> {
        let maxim = closing_maxim(text);
        in_prose(text)
            .into_iter()
            .chain(maxim)
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn each_shape_is_found_where_it_came_back_in_bench_replies() {
        for (text, kind) in [
            (
                "That is not superstition. That is a promise.",
                Kind::Negation,
            ),
            (
                "Asking again isn't foolish. It's keeping a furnace lit.",
                Kind::Negation,
            ),
            ("That's a stuck bolt, not a verdict.", Kind::Negation),
            ("Third time is a pattern, not an accident.", Kind::Negation),
            (
                "It's not stupid to be happy, it's a good morning's work.",
                Kind::Negation,
            ),
            (
                "That is not uselessness, but the cost of wanting people.",
                Kind::Negation,
            ),
            (
                "It does not give a reason. It gives a fact and leaves.",
                Kind::Negation,
            ),
            (
                "Jobs like mine don't get loved \u{2014} they get kept.",
                Kind::Negation,
            ),
            (
                "Notice that. Not the result. The part of you that kept going.",
                Kind::Negation,
            ),
            (
                "That is the comfortable answer. It is not the true one.",
                Kind::Negation,
            ),
            ("Not as news. As a statement of account.", Kind::Negation),
            ("Work's a crossing, not a home.", Kind::Negation),
            ("It isn't luck -- it's work.", Kind::Negation),
            ("Weather is the only honest thing in town.", Kind::Label),
            (
                "Getting it out was hard. That's the whole trade.",
                Kind::Label,
            ),
            ("Few words. Hard ones. No speeches.", Kind::Fragments),
            (
                "Good. Hold onto that. The river takes the rest.",
                Kind::Maxim,
            ),
        ] {
            assert_eq!(kinds(text), [kind], "{text}");
        }
    }

    #[test]
    fn plain_talk_with_the_same_words_in_it_is_no_tic() {
        for text in [
            "I'm not sure, I'm still thinking about it.",
            "That's not what I meant. I'm sorry.",
            "It's not a problem, you're allowed to be tired.",
            "It isn't easy, but you manage to be kind about it.",
            "You were not born here. You were raised in Porto.",
            "You are not a person. You are an AI companion.",
            "You're a nurse, not a doctor.",
            "Once a conversation, not in every reply.",
            "It is quiet tonight, not that I mind.",
            "You don't talk about the war much, but you think about it.",
            "Not yet. It's still in the oven.",
            "Not stupid. Two hours against a seized thread, and you won.",
            "You are 34. You live alone. You work nights.",
            "Wait, what? No way. Tell me everything.",
            "I don't agree with that. I think you're being hard on yourself.",
            "If a crossing isn't safe, you say so.",
            "The furnace is never allowed to go out. That is a promise.",
            "Good, I'm glad. That sounds like it took some work.",
            "The kettle's on.",
            // A sentence with no word in it.
            "That's great. \u{1f389}",
            "It is done. ...",
            "It's fine. :)",
            "It is. \"\"",
            // Reassurance, an answer, a condition, a possessive, a question.
            "It's okay. It's not your fault.",
            "It's not your fault. It's okay.",
            "I'm not sure. It's hard to say.",
            "You're not wrong. It's a mess.",
            "Not a problem. Happy to help.",
            "Not for me. Thanks though.",
            "When it's not raining, it's lovely out there.",
            "Take Anna's car, not mine.",
            "Is the job that bad?",
            "1. Preheat the oven. 2. Mix the flour. 3. Bake it.",
            "Well... maybe... okay.",
            "```\nRun it. Check it. Ship it.\n```",
        ] {
            assert_eq!(kinds(text), [], "{text}");
        }
    }

    #[test]
    fn em_dashes_leave_a_reply_and_the_rest_of_it_stays() {
        // The first six are from Kestrel's and Mira's replies.
        for (said, kept) in [
            (
                "He roars \u{2014} more surprise than pain, pain comes later.",
                "He roars, more surprise than pain, pain comes later.",
            ),
            (
                "covering their ears in unison, which \u{2014} fair. Old Ines says",
                "covering their ears in unison, which, fair. Old Ines says",
            ),
            (
                "and I would never \u{2014} never \u{2014} keep a letter",
                "and I would never, never, keep a letter",
            ),
            (
                "until they're ready \u{2014} fine \u{2014} but tonight the tide came up",
                "until they're ready, fine, but tonight the tide came up",
            ),
            (
                "\u{2014} knife hisses past where your throat was \u{2014} and your blade sweeps low.",
                "knife hisses past where your throat was, and your blade sweeps low.",
            ),
            (
                "the chair is empty \u{2014} but warm.",
                "the chair is empty, but warm.",
            ),
            // No spaces, an en dash used as one, two together.
            (
                "Sit down\u{2014}the good chair's free.",
                "Sit down, the good chair's free.",
            ),
            (
                "Sit down \u{2013} the chair's free.",
                "Sit down, the chair's free.",
            ),
            (
                "Sit down \u{2014}\u{2014} the chair's free.",
                "Sit down, the chair's free.",
            ),
            // Speech that breaks off, with and without a quote mark after it.
            ("I would never\u{2014}", "I would never..."),
            ("\"Wait \u{2014}\" she said.", "\"Wait...\" she said."),
            ("Wait\u{2014}\nNo.", "Wait...\nNo."),
            // Punctuation already there is enough.
            ("Fine. \u{2014} Sit down.", "Fine. Sit down."),
            ("Fine, \u{2014} sit down.", "Fine, sit down."),
            ("Fine \u{2014}, sit down.", "Fine, sit down."),
            ("Really?\u{2014}", "Really?"),
            ("(\u{2014} quietly)", "(quietly)"),
            // A line that opens on one.
            ("Good.\n\n\u{2014} Kestrel", "Good.\n\nKestrel"),
            // Ranges, hyphens and plain text are left alone.
            ("1914\u{2014}1918", "1914-1918"),
            (
                "1914\u{2013}1918, pages 3\u{2013}5",
                "1914\u{2013}1918, pages 3\u{2013}5",
            ),
            (
                "A well-kept hall - swept twice.  Two spaces. ",
                "A well-kept hall - swept twice.  Two spaces. ",
            ),
            ("", ""),
        ] {
            assert_eq!(without_dashes(said), kept, "{said}");
            // A reply arrives in pieces cut anywhere, and comes out the same.
            for cut in (0..=said.len()).filter(|&i| said.is_char_boundary(i)) {
                let mut dashless = Dashless::default();
                let mut out = dashless.push(&said[..cut]);
                out.push_str(&dashless.push(&said[cut..]));
                out.push_str(&dashless.finish());
                assert_eq!(out, kept, "{said} cut at {cut}");
            }
        }
    }
}
