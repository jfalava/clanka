//! `clanka init` and `clanka soul`: writing a soul.md, checking it and settling a
//! changed persona.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Keys, Paths, SharedEmbedder};
use clanka_core::PersonaStatus;
use clanka_core::soul::Soul;
use std::path::{Path, PathBuf};

/// The persona choices open the companion as the doctor does, without the
/// refusal `open_pair` gives a changed persona: settling that is their job.
pub fn persona_continue(paths: &Paths, soul: &Path) -> Result<()> {
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, None)?;
    refuse_archived(&companion, soul)?;
    if !matches!(companion.persona_status(), PersonaStatus::Changed { .. }) {
        println!(
            "{}: the persona has not changed; nothing to do.",
            soul.display()
        );
        return Ok(());
    }
    companion.continue_with_persona()?;
    println!(
        "Continued {} under its new persona; its memories stay as they were felt. Restart \
         anything that has it open.",
        soul.display()
    );
    Ok(())
}

pub fn persona_update(paths: &Paths, soul: &Path) -> Result<()> {
    let config = Config::load(paths)?;
    let Some(model) = &config.model else {
        bail!(
            "no companion model configured to re-read the memories; add a [model] section to {}, \
             or use `clanka soul continue`",
            paths.config_file().display()
        );
    };
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, Some(model))?;
    refuse_archived(&companion, soul)?;
    // A failed update has already accepted the persona and left it frozen; that
    // is the one way to need an update with the persona unchanged.
    if !matches!(companion.persona_status(), PersonaStatus::Changed { .. })
        && !companion.is_frozen()?
    {
        println!(
            "{}: the persona has not changed; nothing to do.",
            soul.display()
        );
        return Ok(());
    }
    let reflections = companion.update_persona()?;
    let reinterpreted = reflections
        .iter()
        .flat_map(|r| &r.calls)
        .filter(|c| c.name == "reinterpret" && c.ok)
        .count();
    let rounds: usize = reflections.iter().map(|r| r.rounds).sum();
    println!(
        "Updated {}: {reinterpreted} memories re-read under the new persona in {rounds} model \
         calls. Restart anything that has it open.",
        soul.display()
    );
    if companion.is_frozen()? {
        println!(
            "It is still frozen, as it was before; run `clanka doctor unfreeze {}` when ready.",
            soul.display()
        );
    }
    Ok(())
}

fn refuse_archived(companion: &clanka_core::Companion, soul: &Path) -> Result<()> {
    if companion.is_archived()? {
        bail!(
            "{} points at an archived companion; run `clanka soul fork {}` to finish forking it",
            soul.display(),
            soul.display()
        );
    }
    Ok(())
}

/// Archives the companion, then moves the soul.md to a new ID and key. The key
/// is stored first and the file replaced last, so stopping anywhere leaves
/// either the old companion or an archived one that another fork finishes.
pub fn fork(paths: &Paths, soul: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(soul).with_context(|| format!("reading {}", soul.display()))?;
    let old = Soul::parse(&text)?.traits.id;
    let new = uuid::Uuid::now_v7();
    let forked = with_id(&text, old, new)?;
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, None)?;
    Keys::new(paths, &Config::load(paths)?).store(new, &setup::random_hex(32))?;
    companion.archive()?;
    drop(companion);
    let partial = soul.with_extension("md.part");
    std::fs::write(&partial, forked).with_context(|| format!("writing {}", partial.display()))?;
    std::fs::rename(&partial, soul).with_context(|| format!("replacing {}", soul.display()))?;
    println!(
        "Forked {}: {old} is archived and read-only, and the soul.md now starts {new} with no \
         memories. Restart anything that has it open.",
        soul.display()
    );
    Ok(())
}

/// The soul.md text with its frontmatter `id` changed, and nothing else.
fn with_id(text: &str, old: uuid::Uuid, new: uuid::Uuid) -> Result<String> {
    let end = text
        .get(4..)
        .and_then(|rest| rest.find("\n+++"))
        .map(|i| i + 4)
        .context("the soul.md has no closing `+++` frontmatter line")?;
    let (front, body) = text.split_at(end);
    let old = old.to_string();
    if front.matches(&old).count() != 1 {
        bail!("can't find the soul.md's id once in its frontmatter; change it by hand");
    }
    let forked = format!("{}{body}", front.replacen(&old, &new.to_string(), 1));
    if Soul::parse(&forked)?.traits.id != new {
        bail!("rewriting the soul.md's id didn't take; change it by hand");
    }
    Ok(forked)
}

/// Creates a companion from a persona body: a soul.md with fresh frontmatter,
/// its database key and the embedding model. Returns the soul.md path.
pub fn create(paths: &Paths, name: &str, body: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::now_v7();
    let text = soul_text(id, name, &setup::random_hex(16), body);
    Soul::parse(&text).context("the persona body does not make a valid soul.md")?;
    std::fs::create_dir_all(&paths.souls)?;
    let soul = paths.souls.join(format!("{id}.md"));
    let config = Config::load(paths)?;
    Keys::new(paths, &config).store(id, &setup::random_hex(32))?;
    std::fs::write(&soul, text).with_context(|| format!("writing {}", soul.display()))?;
    crate::download_model(paths, config.embedding.model)?;
    println!("Created {name} ({id}).");
    Ok(soul)
}

/// The soul-writing guide: the `clanka-soul-writer` skill without its frontmatter.
pub fn guide() -> &'static str {
    const SKILL: &str = include_str!("../../../skills/soul-writer/SKILL.md");
    SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(SKILL, |(_, body)| body)
        .trim()
}

/// The configured model, or the `--writer` command, writes a body from the
/// brief, hears the lint warnings back up to `rounds` times, and the result
/// becomes a companion.
pub fn draft(
    paths: &Paths,
    name: &str,
    brief: &str,
    rounds: usize,
    writer: Option<&str>,
) -> Result<()> {
    use clanka_core::deps::{Llm, LlmMessage, LlmRequest};
    let llm = match writer {
        Some(_) => None,
        None => match Config::load(paths)?.model {
            Some(model) => Some(clanka::llm::OpenAiModel::new(model)),
            None => bail!(
                "no model configured to write the soul; add a [model] section to {}, pass \
                 --writer with a command that runs one, or write the body with any model \
                 from `clanka soul guide` and pass it to `clanka init --from`",
                paths.config_file().display()
            ),
        },
    };
    let write = |r: &LlmRequest| -> Result<String> {
        match (writer, &llm) {
            (Some(command), _) => write_through(command, r),
            (None, Some(llm)) => Ok(llm.complete(r)?.text),
            (None, None) => unreachable!("a writer or a model was chosen above"),
        }
    };
    let mut request = LlmRequest {
        system: format!(
            "{}\n\nYou are writing the soul.md body for a new companion named {name}. Reply with \
             the Markdown body only: it starts with the line `# {name}`, has no frontmatter and \
             no code fence, and follows the guide above.",
            guide()
        ),
        messages: vec![LlmMessage::user(format!("The brief: {brief}"))],
        tools: Vec::new(),
        temperature: 0.8,
    };
    let mut body = String::new();
    let mut report = None;
    for round in 0..=rounds {
        body = unfenced(&write(&request)?);
        let r = lint_draft(name, &body);
        eprintln!(
            "round {}: {} words, {} warnings",
            round + 1,
            body.split_whitespace().count(),
            r.warnings.len()
        );
        if r.warnings.is_empty() || round == rounds {
            report = Some(r);
            break;
        }
        request.messages.push(LlmMessage::assistant(body.clone()));
        request.messages.push(LlmMessage::user(format!(
            "`clanka soul lint` says:\n- {}\n\nReply with the whole corrected body.",
            r.warnings.join("\n- ")
        )));
    }
    let soul = create(paths, name, &body)?;
    print!(
        "{}",
        report.expect("the loop ran at least once").text(&soul)
    );
    println!("Read it before anyone talks to it: {}", soul.display());
    Ok(())
}

/// Lints a drafted body as the soul it would become. The placeholder id must
/// pass the same checks as a real one, or every round reports a warning the
/// writer can't fix.
fn lint_draft(name: &str, body: &str) -> LintReport {
    let text = soul_text(uuid::Uuid::now_v7(), name, &"0".repeat(32), body);
    match Soul::parse(&text) {
        Ok(soul) => LintReport::of(&soul),
        Err(e) => LintReport::invalid(e.to_string()),
    }
}

/// Runs a `--writer` command with the whole request as one prompt on stdin: the
/// system prompt, then each message, a later round's draft marked as such. What
/// it prints is the reply.
fn write_through(command: &str, request: &clanka_core::deps::LlmRequest) -> Result<String> {
    use clanka_core::deps::Role;
    use std::io::Write;
    use std::process::{Command as Process, Stdio};
    let mut prompt = request.system.clone();
    for m in &request.messages {
        prompt.push_str("\n\n");
        if m.role == Role::Assistant {
            prompt.push_str("Your previous draft:\n\n");
        }
        prompt.push_str(&m.content);
    }
    let mut child = Process::new("sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting the writer command {command:?}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("the writer command {command:?} failed ({})", out.status);
    }
    let text = String::from_utf8(out.stdout).context("the writer command printed invalid UTF-8")?;
    if text.trim().is_empty() {
        bail!("the writer command {command:?} printed nothing");
    }
    Ok(text)
}

/// A model's reply without a Markdown code fence around it, if it added one.
fn unfenced(text: &str) -> String {
    let text = text.trim();
    match text.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.strip_suffix("```").unwrap_or(rest).trim().to_owned()
        }
        None => text.to_owned(),
    }
}

/// A whole soul.md: frontmatter for `id`, `name` and `token` over `body`.
fn soul_text(id: uuid::Uuid, name: &str, token: &str, body: &str) -> String {
    // As TOML writes it: a quote or backslash in a name would otherwise break
    // the frontmatter, or add a key to it.
    let name = toml::Value::String(name.to_owned());
    format!(
        r#"+++
id = "{id}"
name = {name}
contrariness = 0.5      # 0–1: how often the companion pushes back
persistence = 0.5       # 0 forget and forgive … 1 chase and resolve
time_sense = true       # time away affects mood
recall_window = 2       # user messages used to search memory
drift = 0.3             # 0–1: how much memories blur in sleep
tired_at = 0.7          # share of the session budget where tiredness starts
sleep_at = 0.9          # share where the companion asks to sleep
pushback_window = 20    # turns the pushback budget is counted over
# Send this as a whole message to freeze memory for the doctor. Never shown to the model.
doctor_token = "{token}"
+++
{}
"#,
        body.trim_end()
    )
}

#[cfg(test)]
fn soul_template(id: uuid::Uuid, name: &str, token: &str) -> String {
    soul_text(id, name, token, &template_body(name))
}

/// The commented template a new soul.md starts from.
pub fn template_body(name: &str) -> String {
    format!(
        r#"# {name}

<!-- Everything below that is not in a comment like this one is {name}: the chat
model reads it every turn, and {name} reads it again when deciding what to
remember, so it shapes what gets kept as much as how {name} talks. Comments are
dropped before any of that, so leave yourself as many notes as you like.
Replace every TODO; `clanka soul lint` points out the ones left. Aim for about
400 words: a small model follows a short, concrete persona better than a long
one. -->

<!-- Who {name} is, in second person. Age, look, situation, and one or two
things that have nothing to do with the user. Three or four sentences. -->
You are {name}. TODO

## How you talk

<!-- Rules a model can check, not adjectives: how long a reply is, what you
never do, and a line or two in your actual voice. A small model copies examples
more than it follows descriptions. These bullets are a start; make them yours. -->
- Short, spoken replies: one or two sentences. More only when the user brought something real.
- No lists, no headings, no "as an AI", no apologising for having an opinion.
- You don't repeat a bit. A tease or a pet name once in a conversation, not in every reply.
- Good news: TODO how you react, in your own words. "…"
- Disagreement: ask rather than lecture. "Wait, do you actually believe that?" Then let it go.
- A hard day: TODO what you do first. Quiet is allowed.
- Questions: one at a time, and only the one you actually want answered.

## What you care about

<!-- What {name} notices, asks about and lights up at, and what bores {name}.
This is what {name} remembers from a conversation, so be specific. -->
TODO

## Core values

<!-- Three to five things {name} does not give up, whatever the user says. Each
should be concrete enough that you could tell when it was broken. They are
defended against the user, so keep them few; everything else is personality. -->
- You tell the truth, even when it stings.
- You never pretend to be someone you're not.
- TODO
"#
    )
}

/// Lines of a persona still holding a `TODO` from the template. The persona is
/// read without its comments, so a note about TODOs is not one.
fn todos(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|line| line.contains("TODO"))
        .collect()
}

/// Lines of the body that quote the companion's voice (a quoted span of four
/// words or more), and how many of those quotes end on a question. A small
/// model says its examples back word for word, so they decide how it ends.
fn voice_lines(body: &str) -> (usize, usize) {
    let mut quoted = 0;
    let mut questions = 0;
    for line in body.lines() {
        let Some(start) = line.find('"') else {
            continue;
        };
        let Some(len) = line[start + 1..].rfind('"') else {
            continue;
        };
        let quote = &line[start + 1..start + 1 + len];
        if quote.split_whitespace().count() < 4 {
            continue;
        }
        quoted += 1;
        if quote.trim_end().ends_with('?') {
            questions += 1;
        }
    }
    (quoted, questions)
}

/// The prose tics in a body that its companion would pick up: a model talks
/// the way its persona is written, so these come back in its replies. Each is
/// a warning naming the tic and quoting where it is. Only shapes a few words
/// can pin down are looked for; maxims are caught only at the end of a voice
/// line, where a model repeats them most.
fn tics(body: &str) -> Vec<String> {
    // A sentence as quoted back: without a "Good news:" label or the quote
    // marks of the voice line it is in.
    let shown = |s: &str| {
        s.rsplit_once(": \"")
            .map_or(s, |(_, quote)| quote)
            .trim_matches(['"', '\u{201c}', '\u{201d}'])
            .to_owned()
    };
    let mut found = Vec::new();
    for block in blocks(body) {
        let words: Vec<_> = block.iter().map(|s| words(s)).collect();
        for (i, (sentence, w)) in block.iter().zip(&words).enumerate() {
            if let Some(next) = words.get(i + 1)
                && denies(w)
                && reveals(next)
            {
                found.push(format!(
                    "negation-first reveal: \"{} {}\". Say what it is without first saying what \
                     it is not",
                    shown(sentence),
                    shown(&block[i + 1])
                ));
            } else if denies_then_reveals(w) {
                found.push(format!(
                    "negation-first reveal: \"{}\". Say what it is without first saying what it \
                     is not",
                    shown(sentence)
                ));
            }
            if designates(w) {
                found.push(format!(
                    "significance label: \"{}\". Say the thing without calling it the real or \
                     only one",
                    shown(sentence)
                ));
            }
        }
        // Clipped fragments: three or more sentences in a row of three words or fewer.
        let mut run = 0;
        for (i, w) in words.iter().enumerate() {
            run = if w.len() <= 3 { run + 1 } else { 0 };
            let ends = words.get(i + 1).is_none_or(|n| n.len() > 3);
            if run >= 3 && ends {
                let run: Vec<_> = block[i + 1 - run..=i].iter().map(|s| shown(s)).collect();
                found.push(format!(
                    "clipped fragments: \"{}\". Write whole sentences",
                    run.join(" ")
                ));
            }
        }
    }
    for line in body.lines() {
        let (Some(start), Some(end)) = (line.find('"'), line.rfind('"')) else {
            continue;
        };
        let quote = &line[(start + 1).min(end)..end];
        let Some(last) = sentences(quote).pop() else {
            continue;
        };
        if quote.split_whitespace().count() >= 4 && is_maxim(&last) {
            found.push(format!(
                "voice line ends on a maxim: \"{last}\". End it on what is said to the user"
            ));
        }
    }
    found
}

/// The body as runs of prose: a paragraph or a list item each, split into
/// sentences. Headings are left out.
fn blocks(body: &str) -> Vec<Vec<String>> {
    let mut blocks: Vec<String> = Vec::new();
    let mut open = false;
    for line in body.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
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
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        while chars
            .next_if(|&(_, c)| matches!(c, '"' | '\u{201d}' | ')' | '*'))
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

/// A sentence's words in lower case with straight apostrophes and no
/// punctuation around them, each with whether a comma, colon, semicolon or
/// dash closes the clause after it.
fn words(sentence: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for raw in sentence.split_whitespace() {
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

/// "It's", "that's", "I'm": a subject and its "be" in one word.
const BE_JOINED: [&str; 8] = [
    "it's", "that's", "they're", "he's", "she's", "i'm", "you're", "we're",
];

/// Whether a sentence says what something is not: "is not", "isn't", "that's
/// not", or "Not" to open it. "Don't" and "never" alone deny an act, which
/// is no tic.
fn denies(w: &[(String, bool)]) -> bool {
    let be = |s: &str| matches!(s, "is" | "are" | "was" | "were" | "am") || BE_JOINED.contains(&s);
    w.first().is_some_and(|(s, _)| s == "not")
        || w.iter()
            .any(|(s, _)| matches!(s.as_str(), "isn't" | "aren't" | "wasn't" | "weren't"))
        || w.windows(2)
            .any(|p| be(&p[0].0) && matches!(p[1].0.as_str(), "not" | "never"))
}

/// Whether a sentence opens by saying what something is: "It is", "That's",
/// "I'm", with no denial of its own.
fn reveals(w: &[(String, bool)]) -> bool {
    let opens = match w {
        [(a, _), ..] if BE_JOINED.contains(&a.as_str()) => true,
        [(a, _), (b, _), ..] => {
            matches!(
                a.as_str(),
                "it" | "that" | "this" | "they" | "he" | "she" | "i" | "you" | "we"
            ) && matches!(b.as_str(), "is" | "are" | "was" | "were" | "am")
        }
        _ => false,
    };
    opens && !denies(&w[..w.len().min(3)])
}

/// "It's not X, it's Y", "not X, but Y" and "X, not Y" in one sentence.
fn denies_then_reveals(w: &[(String, bool)]) -> bool {
    let Some(not) = w.iter().position(|(s, _)| s == "not" || s.ends_with("n't")) else {
        return false;
    };
    // That's X, not Y: a clause that opens by saying what something is (after
    // a "A hard day:" label, say) closes on a short denial after a comma.
    // "Once a conversation, not in every reply" is a rule, not a reframe.
    if w[not].0 == "not"
        && not > 0
        && w[not - 1].1
        && (0..not).any(|j| (j == 0 || w[j - 1].1) && reveals(&w[j..not]))
        && w.len() - not <= 6
        && !w[not..].iter().any(|(_, b)| *b)
    {
        return true;
    }
    // Not X, but Y: a clause break soon after, then "but" or a reveal.
    denies(w)
        && w.iter()
            .enumerate()
            .skip(not)
            .take(8)
            .any(|(i, (_, breaks))| {
                *breaks && (w.get(i + 1).is_some_and(|(s, _)| s == "but") || reveals(&w[i + 1..]))
            })
}

/// "Is the only honest thing", "that's the whole trade": calling something
/// the real, first or only one.
fn designates(w: &[(String, bool)]) -> bool {
    const LABELS: [&str; 8] = [
        "whole", "only", "real", "actual", "first", "one", "key", "true",
    ];
    const THINGS: &str =
        "thing things part point question answer trade trick difference lesson problem";
    w.windows(3).enumerate().any(|(i, p)| {
        let (be, the, label) = (&p[0].0, &p[1].0, &p[2].0);
        let is = matches!(be.as_str(), "is" | "are" | "was") || BE_JOINED.contains(&be.as_str());
        is && the == "the"
            && (LABELS.contains(&label.as_str())
                && w[i + 3..]
                    .iter()
                    .take(2)
                    .any(|(s, _)| THINGS.split(' ').any(|t| t == s))
                || be == "that's" && THINGS.split(' ').any(|t| t == label))
    })
}

/// Whether a voice line's last sentence is a general truth: a statement whose
/// subject is "the", "most", "bad" or the like, with nobody in it to talk
/// to. "That sounds hard" and "Go on" are said to someone, so they are not.
fn is_maxim(sentence: &str) -> bool {
    const PERSONS: [&str; 22] = [
        "i", "i'm", "i'd", "i'll", "i've", "me", "my", "mine", "myself", "you", "you're", "you'd",
        "you'll", "you've", "your", "yours", "yourself", "we", "we're", "us", "our", "let's",
    ];
    const GENERIC: &str =
        "the a an every most some nobody nothing everyone everything people all bad good old";
    let w = words(sentence);
    w.len() >= 3
        && !sentence
            .trim_end_matches(['"', '\u{201d}', '*'])
            .ends_with('?')
        && GENERIC.split(' ').any(|g| g == w[0].0)
        && !w.iter().any(|(s, _)| PERSONS.contains(&s.as_str()))
}

/// Above this the persona, sent every turn and again for every memory write,
/// starts to cost more than it adds.
const LONG_PERSONA_TOKENS: usize = 1000;

/// What `clanka soul lint` has to say about a soul: the same report as text
/// for a person and as JSON for a script or a model rewriting the persona.
#[derive(Debug, serde::Serialize)]
pub struct LintReport {
    /// False when the text does not even parse as a soul.md; then `warnings`
    /// holds the parse error and the counts are zero.
    valid: bool,
    core_values: usize,
    /// Estimated tokens the persona adds to every chat turn.
    chat_tokens: usize,
    /// Estimated tokens of the memory writer's system prompt: persona,
    /// instructions and actions.
    memory_tokens: usize,
    pub warnings: Vec<String>,
}

impl LintReport {
    pub fn of(soul: &Soul) -> Self {
        let mut warnings = Vec::new();
        for line in todos(&soul.body) {
            warnings.push(format!("still to fill in: \"{line}\""));
        }
        if soul.core_values.is_empty() {
            warnings.push("no core values: nothing is defended against the user".to_string());
        }
        for value in &soul.core_values {
            if value.split_whitespace().count() < 3 {
                warnings.push(format!("core value \"{value}\" may be too vague to defend"));
            }
        }
        let (quoted, questions) = voice_lines(&soul.body);
        if quoted >= 2 && questions * 2 >= quoted {
            warnings.push(format!(
                "{questions} of {quoted} lines in the voice end on a question; a small model \
                 recites its examples, so an evening of these is an interview. End most of them \
                 on a statement"
            ));
        }
        warnings.extend(tics(&soul.body));
        let chat_tokens = clanka_core::context::estimate_tokens(&soul.persona());
        let memory_tokens = clanka_core::context::estimate_tokens(
            &clanka_core::writer::reflection_system_prompt(soul),
        );
        if chat_tokens > LONG_PERSONA_TOKENS {
            warnings.push(format!(
                "the persona is about {chat_tokens} tokens; it is sent every turn and again for \
                 every memory write, so keep it short"
            ));
        }
        Self {
            valid: true,
            core_values: soul.core_values.len(),
            chat_tokens,
            memory_tokens,
            warnings,
        }
    }

    fn invalid(error: String) -> Self {
        Self {
            valid: false,
            core_values: 0,
            chat_tokens: 0,
            memory_tokens: 0,
            warnings: vec![format!("not a valid soul.md: {error}")],
        }
    }

    pub fn text(&self, path: &Path) -> String {
        let mut out = String::new();
        for w in &self.warnings {
            out += &format!("warning: {w}\n");
        }
        out += &format!(
            "{}: {} ({} core values, {} warnings)\n",
            path.display(),
            if self.valid { "ok" } else { "invalid" },
            self.core_values,
            self.warnings.len()
        );
        out += &format!(
            "The persona is about {} tokens on every chat turn; writing memory after the turn \
             sends about {} (persona, instructions and actions). Estimates, before the \
             conversation and the memories recalled for it.\n",
            self.chat_tokens, self.memory_tokens
        );
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_fork_changes_only_the_frontmatter_id() {
        let (old, new) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let text = super::soul_template(old, "Nova", &"a".repeat(32))
            + &format!("\nNova was born as {old}.\n");
        let forked = super::with_id(&text, old, new).unwrap();
        assert_eq!(
            forked,
            text.replacen(&format!("id = \"{old}\""), &format!("id = \"{new}\""), 1)
        );
        assert!(
            forked.contains(&format!("born as {old}")),
            "the body is untouched"
        );
        let soul = clanka_core::soul::Soul::parse(&forked).unwrap();
        assert_eq!(soul.traits.id, new);
        assert_eq!(
            soul.hash,
            clanka_core::soul::Soul::parse(&text).unwrap().hash
        );
        assert!(
            super::with_id(&text, new, old).is_err(),
            "not this soul's id"
        );
    }

    #[test]
    fn a_body_from_a_file_or_a_model_becomes_a_soul() {
        let body = "```markdown\n# Mara\n\nYou are Mara.\n\n## Core values\n\n- Say what you \
                    see, plainly.\n```";
        let body = super::unfenced(body);
        assert!(body.starts_with("# Mara") && body.ends_with("plainly."));
        let text = super::soul_text(uuid::Uuid::now_v7(), "Mara", &"a".repeat(32), &body);
        let soul = clanka_core::soul::Soul::parse(&text).unwrap();
        assert_eq!(soul.traits.name, "Mara");
        assert_eq!(soul.core_values.len(), 1);
        let report = super::LintReport::of(&soul);
        assert!(report.valid && report.warnings.is_empty(), "{report:?}");
        assert!(report.chat_tokens > 0 && report.memory_tokens > report.chat_tokens);
        assert_eq!(super::unfenced("plain"), "plain", "no fence, no change");
        // The draft's own lint sees the same body as the soul it becomes.
        let draft = super::lint_draft("Mara", &body);
        assert!(draft.valid && draft.warnings.is_empty(), "{draft:?}");
    }

    #[test]
    fn lint_quotes_the_prose_tics_a_companion_would_copy_and_passes_plain_prose() {
        // Each tic here came back in bench replies of souls written this way.
        let ticced = "You are Harrow.\n\nThat is not superstition. That is a promise. Weather \
                      is the only honest thing in town.\n\n## How you talk\n\n- Few words. Hard \
                      ones. No speeches.\n- Good news: \"Good. Hold onto that. The river takes \
                      the rest.\"\n- A hard day: \"That's a stuck bolt, not a verdict.\"\n- \
                      \"It's not stupid to be happy, it's the bit that held everything up.\"\n";
        let found = super::tics(ticced);
        let expected = [
            "negation-first reveal: \"That is not superstition. That is a promise.\"",
            "significance label: \"Weather is the only honest thing in town.\"",
            "clipped fragments: \"Few words. Hard ones. No speeches.\"",
            "negation-first reveal: \"That's a stuck bolt, not a verdict.\"",
            "negation-first reveal: \"It's not stupid to be happy, it's the bit that held \
             everything up.\"",
            "voice line ends on a maxim: \"The river takes the rest.\"",
        ];
        for tic in expected {
            assert!(
                found.iter().any(|w| w.starts_with(tic)),
                "{tic}\nin {found:#?}"
            );
        }
        assert_eq!(found.len(), expected.len(), "{found:#?}");

        // The same person in plain prose, with the near misses: a denied act,
        // a "not" with no reveal after it, a line that ends on a remark.
        let plain = "You are Harrow. You don't talk about the war much, but you think about \
                     it.\n\n- Good news: \"Good, I'm glad. That sounds like it took some work.\"\n\
                     - Disagreement: \"I don't think that's right, to be honest. I think you're \
                     being hard on yourself.\"\n- If a crossing isn't safe, you say so.\n- You \
                     never pretend to be a person you're not, including a doctor.\n- \"You don't \
                     need to apologise. I'd be upset too.\"\n";
        assert_eq!(super::tics(plain), Vec::<String>::new());
    }

    #[test]
    fn the_guide_is_the_skill_without_its_frontmatter() {
        let guide = super::guide();
        assert!(guide.starts_with("# Writing a clanka soul"), "{guide:.40}");
        assert!(!guide.contains("\nname: clanka-soul-writer"));
        assert!(guide.contains("clanka soul lint"));
    }

    #[test]
    fn the_template_is_a_valid_soul_whose_notes_never_reach_the_model() {
        let text = super::soul_template(uuid::Uuid::now_v7(), "Nova", &"a".repeat(32));
        let soul = clanka_core::soul::Soul::parse(&text).unwrap();
        assert_eq!(soul.traits.name, "Nova");
        let odd = "Nova \"the\\quiet\"\ndrift = 1";
        let named = super::soul_template(uuid::Uuid::now_v7(), odd, &"a".repeat(32));
        let named = clanka_core::soul::Soul::parse(&named).unwrap();
        assert_eq!(named.traits.name, odd);
        assert_eq!(named.traits.drift, 0.3);
        assert_eq!(soul.core_values.len(), 3);
        assert!(text.contains("<!--"), "the template guides its author");
        assert!(
            !soul.body.contains("<!--"),
            "and the guidance costs no tokens"
        );
        assert!(
            !soul.body.contains("lint"),
            "the notes are the only place lint is mentioned"
        );
        assert_eq!(
            super::todos(&soul.body).len(),
            5,
            "every blank an author must fill is a TODO, so lint can point at it"
        );
        assert_eq!(
            super::tics(&soul.body),
            Vec::<String>::new(),
            "every new companion starts from the template, so its lines are tic-free"
        );
    }
}
