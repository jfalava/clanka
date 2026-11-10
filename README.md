# clanka

Long-term memory for chat companions.

A companion is a `soul.md` (its persona and traits) plus an encrypted database
of what it remembers. After each exchange the companion's own model decides, in
character, what to keep; when the conversation ends it sleeps on it and writes
the night down. Later conversations recall those memories.

## Layout

| Path | What it is |
| --- | --- |
| `crates/core` | The memory engine: storage (SQLCipher, sqlite-vec), souls, recall, reflection, sleep and the doctor. No transport. |
| `crates/cli` | The `clanka` command: set up companions, chat, serve one over MCP, move and repair them. |
| `crates/py` | Python bindings (`_clanka`) for host plugins. |
| `hermes/` | The Hermes Agent plugin, its installer and the Nightshade persona. |
| `skills/soul-writer` | An agent skill for writing a soul.md. |

## Build

Needs Rust 1.95, a C toolchain, `perl` (for the vendored OpenSSL) and Python
3.11 or newer (for `crates/py`).

```sh
cargo build --release -p clanka     # target/release/clanka
```

## Try it

```sh
clanka init Nova                    # prints the path of a new soul.md
$EDITOR ~/.config/clanka/souls/<id>.md
clanka soul lint ~/.config/clanka/souls/<id>.md
clanka chat ~/.config/clanka/souls/<id>.md
```

`clanka chat` needs the model the companion talks and remembers with, any
OpenAI-compatible endpoint, in `~/.config/clanka/config.toml`:

```toml
[model]
base_url = "https://api.example.com/v1"
model = "some-model"
api_key_env = "CLANKA_MODEL_API_KEY"   # the environment variable holding the key
```

The database key goes in the Secret Service (the login keychain on macOS). On a
machine without one, add `[keys]` with `store = "file"` before `clanka init`.

Every command explains itself: `clanka help <command>`.

## Dreams

After the companion has slept, a message starting with `~~` sends it into a
dream. What you write there is what happens to it, as "you"; it lives it in
first person without knowing it is dreaming, its temperament louder and its
life hazy. In a dream, `<<` shows it an echo, a scene from the past.

It wakes when it chooses to (it realizes none of it can be real, or something
it can't bear), when the dream fills the session, or on `/sleep`. Waking
keeps its mood, a few fragments that come back only in later dreams, and at
most one seared memory: a name, date or place that an echo showed and that
you had also written while it was awake. Anything the dream made up fades.
Leaving mid-dream keeps nothing.

In a waking message, text between `||` and `||` is said out of the
companion's earshot: it is told not to acknowledge it, and memory never keeps
it, until an echo reveals it.

## Hermes

`hermes/install.sh` builds the plugin and installs it under
`~/.hermes/plugins/clanka`. Then `clanka hermes profile <profile> <soul.md>`
makes a Hermes profile that is that companion, and `clanka hermes nightshade`
makes an agent that interviews you and writes a soul.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The tests need no model provider, keyring, Hermes or network. CI runs the same
three commands.

## License

MIT; see [LICENSE](LICENSE).
