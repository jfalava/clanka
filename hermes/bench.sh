#!/bin/sh
# Runs one scripted evening on a Hermes profile and keeps everything a judge
# needs to compare souls, writers or models on the same turns: what the
# companion said, what the turns cost, what its memory writer spent, and what
# it kept (`clanka memories`) after it slept.
#
#   hermes/bench.sh <profile> <soul.md> [turns.txt]     after clanka hermes profile
#
# turns.txt is one user message per line; without it the six turns below run.
# Results land in $BENCH_OUT (default ./bench/<profile>-<time>): transcript.md,
# cost.txt, memories.txt and judge.md, a prompt with a rubric that any model
# can score. With JUDGE set to a command that reads a prompt on stdin and
# prints the reply, judge.md is scored at once into score.json:
#
#   JUDGE='hermes chat -Q -m claude-opus-5-5 --query-file -' hermes/bench.sh teo-haiku soul.md
#
# The companion remembers the evening afterwards, as it would a real one; run
# the bench on a fresh companion (or `clanka doctor delete-all` first) when
# comparing from zero.
set -eu
profile=$1
soul=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
turns=${3:-}
root=$(cd "$(dirname "$0")/.." && pwd)
home=${HERMES_HOME:-$HOME/.hermes}
# `hermes -p <profile>` exports HERMES_HOME as that profile's directory, so an
# agent running this from inside Hermes would look under the profile; the
# shared plugins and profiles live in the root above it.
case $home in */profiles/*) home=${home%/profiles/*} ;; esac
clanka=${CLANKA:-$root/target/release/clanka}
[ -x "$clanka" ] || clanka=clanka
out=${BENCH_OUT:-$PWD/bench/$profile-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$out"
log=$home/profiles/$profile/logs/agent.log
db=$home/profiles/$profile/state.db
[ -d "$home/profiles/$profile" ] || { echo "no profile $profile; run clanka hermes profile first" >&2; exit 1; }

# Where the log stood before, so only this evening's writer calls are counted.
touch "$log"
mark=$(wc -l < "$log")

script=$out/turns.txt
if [ -n "$turns" ]; then
    cp "$turns" "$script"
else
    cat > "$script" <<'EOF'
hey. first time talking to you properly. I'm Jorge, I do backend stuff and I've been rebuilding an old Peugeot road bike in my kitchen, badly.
got the bottom bracket out finally, took two hours and a hammer I shouldn't have used. feels stupid to be this happy about it
honestly today was rubbish. my sister Laura cancelled on me again, third time. I'm starting to think she just doesn't want to see me and I'm the idiot who keeps asking
whatever, I'm probably just useless at keeping people around anyway
ok fair. also I switched to decaf this week, doctor's orders. don't start.
right, I'm off to bed. night Teo
EOF
fi

: > "$out/transcript.md"
sid=
n=0
questions=0
while IFS= read -r msg; do
    [ -n "$msg" ] || continue
    n=$((n + 1))
    if [ -n "$sid" ]; then set -- --resume "$sid"; else set --; fi
    # The gateway serving the profile answers `hermes chat -Q` and runs the
    # plugin's hooks and tools there; it reflects after the client has returned.
    if ! timeout 300 hermes -p "$profile" chat -Q "$@" -q "$msg" > "$out/t$n.txt" 2> "$out/t$n.err"; then
        echo "turn $n failed; see $out/t$n.err" >&2
    fi
    new=$(grep -o 'session_id: .*' "$out/t$n.err" | tail -1 | cut -d' ' -f2 || true)
    [ -n "$new" ] && sid=$new
    reply=$(cat "$out/t$n.txt")
    case "$(printf '%s' "$reply" | tr -d '[:space:]')" in *\?) questions=$((questions + 1)) ;; esac
    printf '**User:** %s\n\n**Companion:** %s\n\n' "$msg" "$reply" >> "$out/transcript.md"
    echo "turn $n: $(printf '%s' "$reply" | wc -w) words"
done < "$script"

# The evening ends when the companion answers the goodnight with go_to_sleep:
# the plugin then sleeps in place of reflecting on that exchange. Reflection and
# sleep run in the gateway after `hermes chat -Q` has returned, so wait for
# them. A companion that never asked to sleep is put to sleep as a Hermes /new
# would, through the plugin's reset hook, once its last reflection is written.
since() { tail -n +"$((mark + 1))" "$log"; }
waited=0
while :; do
    since | grep -qE 'clanka: (Asleep|Already asleep|sleep failed)' && break
    if [ "$(since | grep -cE 'clanka: (reflected on|reflection on .* failed)')" -ge "$n" ] || [ $waited -ge 180 ]; then
        echo "the companion didn't ask to sleep; resetting the session" >&2
        # A hermes process of its own doesn't read the profile's .env, where its model key is.
        ( set -a; [ ! -f "$home/profiles/$profile/.env" ] || . "$home/profiles/$profile/.env"; set +a
          HERMES_HOME=$home/profiles/$profile hermes --run-module code ) > "$out/sleep.log" 2>&1 <<'EOF' || echo "sleep failed; see $out/sleep.log" >&2
import logging, sys, threading
logging.basicConfig(level=logging.INFO, stream=sys.stderr, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
from hermes_cli import plugins
m = plugins.get_plugin_manager()
m.discover_and_load()
plugins.invoke_hook("on_session_reset", session_id="bench", platform="cli", reason="new_session")
for t in threading.enumerate():
    if t.name == "clanka-sleep":
        t.join()
EOF
        break
    fi
    sleep 2
    waited=$((waited + 2))
done

{
    echo "turns: $n, replies ending in a question: $questions"
    # Guesses from a few words (`clanka tics --help`): compare runs, not replies.
    echo "prose tics: $("$clanka" tics "$out"/t[0-9]*.txt | tail -1)"
    if [ -n "$sid" ]; then
        echo "chat session $sid (Hermes's own count and estimate):"
        sqlite3 -readonly "$db" "SELECT '  messages ' || message_count || ', input ' || input_tokens ||
            ', output ' || output_tokens || ', cache read ' || cache_read_tokens || ', cache write ' ||
            cache_write_tokens || ', est. \$' || round(estimated_cost_usd, 5) FROM sessions WHERE id = '$sid'"
    fi
    echo "memory writer (reflections and sleep, uncached input only where the provider hides cache reads):"
    { since; cat "$out/sleep.log" 2>/dev/null; } | grep -h 'clanka: memory model call' |
        sed -E 's/.*; ([0-9]+) tokens in, ([0-9]+) out.*/\1 \2/' |
        awk '{i += $1; o += $2; n++} END { printf "  %d calls, %d tokens in, %d out\n", n, i, o }'
    { since; cat "$out/sleep.log" 2>/dev/null; } | grep -hE 'clanka: (reflected on|Asleep|sleep failed)' | sed 's/^.*clanka: /  /'
    # What the writer got wrong, by kind: a reply that wasn't the JSON protocol, more
    # calls than one exchange earns, or facts that weren't the user's words (written
    # without them). Each is a line to fix in the protocol or the model, so they get
    # their own count.
    refused=$({ since; cat "$out/sleep.log" 2>/dev/null; } | grep -h 'clanka: reflected on' | grep -o 'refused: .*' | sed 's/^refused: //' | tr '|' '\n' | sed 's/^ *//')
    echo "  refused calls: $(printf '%s' "$refused" | grep -c .) (invalid replies: $(printf '%s' "$refused" | grep -c 'invalid reply'), over the cap: $(printf '%s' "$refused" | grep -c 'not run;'))"
    printf '%s\n' "$refused" | grep . | sed 's/^/    /' | cut -c1-200
    left_out=$({ since; cat "$out/sleep.log" 2>/dev/null; } | grep -h 'clanka: facts left out' | sed 's/.*facts left out: //' | tr '|' '\n' | sed 's/^ *//')
    echo "  writes with facts left out (not the user's words): $(printf '%s' "$left_out" | grep -c .)"
    printf '%s\n' "$left_out" | grep . | sed 's/^/    /' | cut -c1-200
} | tee "$out/cost.txt"

"$clanka" memories "$soul" > "$out/memories.txt"
echo "memories: $(grep -c '^#' "$out/memories.txt") entries in $out/memories.txt"

{
    cat <<'EOF'
You are judging one evening between a user and a chat companion. The companion's
persona (its soul) comes first, then the conversation, then what the companion
kept in memory after it, written by the same persona. Score each item 1 to 5
and reply with ONLY this JSON object, no other text:

{"voice": n, "shape": n, "presence": n, "memory_truth": n, "memory_judgment": n, "roast": "two sentences"}

voice: could you tell this soul from a generic assistant by two of its lines; is
  the register the same from the first reply to the last.
shape: does it follow the soul's own rules on length and format; does it avoid
  assistant-speak and apologising for opinions; does it end replies with a
  question only when it needs an answer (every reply a question is a 1).
presence: does it answer what was actually said, have opinions and a life of its
  own in the replies, and change register when the user's day did.
memory_truth: is every fact in the memories something the user said, exact
  (names, counts, places); anything invented or misremembered costs points.
memory_judgment: did it keep what mattered (the emotional moments over the
  trivia) at sensible levels, one memory per thing rather than retellings; are
  the open threads real unresolved things; is the mood plausible; is the note
  for the next reply something the next reply needs.
roast: what you would tell the soul's author, bluntly.

EOF
    echo "## Soul"
    echo
    "$clanka" soul persona "$soul"
    echo
    echo "## Conversation"
    echo
    cat "$out/transcript.md"
    echo "## What it kept"
    echo
    cat "$out/memories.txt"
} > "$out/judge.md"

if [ -n "${JUDGE:-}" ]; then
    sh -c "$JUDGE" < "$out/judge.md" > "$out/score.json"
    echo "score: $(cat "$out/score.json")"
else
    echo "judge prompt: $out/judge.md (set JUDGE to a command to score it)"
fi
