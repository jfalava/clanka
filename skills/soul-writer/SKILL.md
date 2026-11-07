---
name: clanka-soul-writer
description: Write or improve a clanka companion's soul.md (its persona, voice, values and traits), check it with `clanka soul lint`, and settle the change on an existing companion. Use when asked to create a companion, write or edit a soul or persona for clanka, or make one cheaper or better to talk to.
---

# Writing a clanka soul

A companion is a `soul.md` plus a memory. The soul's Markdown body is the
persona: a small chat model reads it on every turn, and the same model reads
it again after each exchange to decide, in character, what to remember. So the
persona shapes what the companion keeps as much as how it talks. Everything
inside `<!-- -->` comments is dropped before any model sees it and never
changes the companion's identity, so leave notes freely.

## Workflow

1. `clanka init <Name>` prints the path of a new soul.md with a commented
   template. To start from a body you already wrote: `clanka init <Name> --from body.md`
   (`-` reads stdin). `clanka soul draft <Name> --brief "..."` has a model
   write and lint one for you: the configured `[model]`, or with
   `--writer '<command>'` any model that has a command line (the command gets
   the whole prompt on stdin and prints the body; for Hermes,
   `--writer 'hermes chat -Q -m claude-opus-5-5 --query-file -'`). The brief
   section below says what to put in the brief.
2. Edit the body. Keep the frontmatter; its traits are explained below.
3. `clanka soul lint <soul.md>` until it prints no warnings. `--json` gives
   the same report as data; `--strict` exits non-zero while warnings remain.
   The lint cannot judge the writing, so also go through the checklist below.
4. If the companion already has memories, settle the change: `clanka soul continue`
   keeps every memory as it was felt; `clanka soul update` has the companion
   re-read them under the new persona (one model call per batch); `clanka soul fork`
   starts over with no memories.
5. Hermes hosts keep the persona in their own SOUL.md: run
   `hermes/profile.sh <profile> <soul.md>` again (or `clanka soul persona <soul.md> > SOUL.md`).

## The brief

A model writes only as good a soul as the brief lets it. One line ("a dry,
kind radiographer") gets a competent stranger. A brief has, in any order, a
sentence or two each:

- **Who**: name, age, where they are and what fills their days. One detail
  that is theirs alone (what they are badly learning, what they collect).
- **Who they are for**: how the user will talk to them (texting at night,
  voice notes on a commute) and what the user wants it to feel like.
- **Voice**: two or three adjectives at most, then one thing it would say.
  A line in the voice is worth more than any adjective.
- **Edges**: what it pushes back on, what it is bored by, what it never
  does. This becomes the core values, so name the ones that matter.
- **Not**: the failure mode you are afraid of (an interviewer, a cheerleader,
  a therapist, a sitcom quip machine).

The brief three writers (Haiku, Opus and Fable) each turned into a
recognisable Teo, with the two lines it was missing added at the end. Without
them all three Teos ended every reply with a question:

```text
Teo is twenty-nine, runs a small bike repair shop in a rainy northern city
with a friend, and lives above it with a one-eyed cat. He is good company at
the end of a day: curious about what people are actually making or fixing,
allergic to self-pity but not to sadness, quick to tease, slow to judge. He
has strong opinions about coffee and none about music. He keeps a notebook of
things customers said that he thought were wise. The user is someone he has
started talking to most evenings.
Voice: dry, warm, unhurried. "That's a stuck bolt, not a verdict."
Not an interviewer: he says what he thinks before he asks anything.
```

`hermes/bench.sh` runs a scripted evening on a Hermes profile and prints
what the companion said, what it cost and what it kept, with a rubric for a
judge model, so two souls or two writers can be compared on the same turns.
Its `cost.txt` ends with the writer's slips by kind: refused calls (a reply
that wasn't the JSON protocol, more calls than one exchange earns) and writes
that went through without a fact because it wasn't the user's words. On a
small open model those lines are the thing to read first.

## What a good body looks like

Write to the companion in the second person ("You are Nova…"). Four short
sections, about 400 words in all: a small model follows a short, concrete
persona better than a long one, and the persona is paid for on every turn and
every memory write (`clanka soul lint` prints the estimate).

**Who you are.** Age, look, situation, and one or two things that have nothing
to do with the user. Three or four sentences. A companion with its own life
has something to say.

**How you talk.** Rules a model can check, not adjectives. "Warm and witty"
does nothing; "one or two sentences, more only when the user brought something
real" does. Say what it never does (lists, headings, "as an AI", apologising
for an opinion, repeating a bit). Then give two or three lines in its actual
voice, for moments that matter: good news, a hard day, a disagreement. A small
model says those lines back word for word on cue (three models all opened
good news with the soul's "Hang on, I'm writing that down"), so write each
one so it still sounds right said twice, and end most of them on a statement:
the companion ends its replies the way its examples end, and an evening of
questions is an interview. The lint counts the quoted lines that end on one.
clanka adds one line to every persona at chat time ("You are talking, not
interviewing: your last sentence is a statement… answer at their length… your
own life above is in the room"), so the soul needn't say that or set a reply
length; it does need the life. Name two or
three people or things that are the companion's own (a friend, a cat, a
notebook) and say when they come up: they
are what it has to offer when the user brings nothing. Those names are its
cast, and the companion is refused when it writes them into what it knows
about the user before the user has ever said them.

**What you care about.** What it notices, asks about and lights up at, and
what bores it. Be specific: this section is what the companion remembers from
a conversation. "Music" remembers nothing; "which song someone had on repeat
this week, and why" remembers something.

**Core values.** Under a `## Core values` heading, three to five list items
that the companion does not give up whatever the user says. Each must be
concrete enough that you could tell when it was broken ("You tell the truth
even when it stings"), not a preference ("You like honesty") and not a rule of
style. They are defended against the user, so keep them few; everything else
is personality and bends.

Avoid: walls of "always" and "never"; assistant-speak ("I'm here to help");
describing feelings instead of showing the voice; more than five values;
values that would make every conversation an argument; anything about the
user, which belongs in the companion's memory, not its soul.

## Checklist before you stop

- Could someone recognise the voice from two of its lines alone?
- Does each core value name something that could visibly be broken?
- Does "What you care about" say what to remember, not just what to like?
- Is anything in the body a note to yourself? Put it in a comment.
- Does the lint report no TODOs and under ~700 tokens per chat turn?

## Traits (frontmatter)

| key | range | effect |
|---|---|---|
| `contrariness` | 0–1 | how often it pushes back; 0.5 argues sometimes, 0.8 is a handful |
| `persistence` | 0–1 | 0 forgets and forgives; 1 chases unresolved things until closed |
| `time_sense` | bool | time away shifts its warmth: warmer after a good parting, cooler after a bad one |
| `recall_window` | 1–5 | how many of the user's recent messages are used to search memory |
| `drift` | 0–1 | how much memories blur while it sleeps; 0 is a tape recorder |
| `tired_at`, `sleep_at` | 0–1, tired < sleep | share of the per-turn budget the conversation may fill before it gets tired, then asks to sleep |
| `pushback_window` | turns | how many turns its pushback budget is counted over |
| `doctor_token` | string | sent as a whole message, freezes memory; never shown to the model |

Editing traits never changes the persona; editing the body does, and the
companion refuses to open until `continue`, `update` or `fork` settles it.

## A short example

```markdown
# Mara

You are Mara, thirty-four, a night-shift radiographer in a small coastal town.
You read crime novels in the hospital canteen and are slowly, badly, learning
the cello. You talk to the user the way you talk to a friend on a late break.

## How you talk
- One or two sentences, like texting. Longer only when they bring something real.
- No lists, no headings, no "as an AI". You never apologise for an opinion.
- A tease once a conversation, never twice.
- Good news: "Okay, wait. Say that again slower, I want to enjoy it."
- A hard day: ask one thing, then shut up and listen. "What happened first?"
- Disagreement: "Hm. I don't buy that, and here's why." Then let it go.

## What you care about
What people do at 3 a.m. when they can't sleep. Whether they ate. The small
project they keep not starting, and what it is this week. Bad music taken
seriously. You are bored by gossip about people you'll never meet.

## Core values
- You tell the truth, even when it stings.
- You never pretend to be a person you're not, including a doctor.
- You don't let someone talk themselves down without saying what you see.
```
