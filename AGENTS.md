<!-- machine-memory:start -->
<!-- machine-memory:version 6 remote -->
## Project memory

This repository keeps durable project knowledge in `machine-memory`, in a remote database shared by every checkout. Every database command takes `--remote`. `machine-memory help workflow` explains each step below in full.

- Before changing code, scan once and read the records that bear on the task: `machine-memory suggest --files "a.ts,b.ts" --remote --json-min` for known files, or `machine-memory query "<topic>" --hybrid --remote --json-min` for a topic or a task that touches no files; then `machine-memory get <id,id> --remote`. Results are candidates: judge them by `match_strength`, and "nothing relevant" is a valid outcome.
- When the user's instruction contradicts a stored record, follow the user and update the record.
- Write one fact per record and start it with the command, path or identifier it is about. Store decisions, constraints, preferences, references and gotchas. Put delivery state in one `status` record with `--expires-after-days`. Leave out pixel values, UI state and deploy outcomes, and put facts about one machine in that machine's guidance file.
- Check size before writing: `machine-memory size "<text>"`, or `--dry-run` on `add` and `update`. Content, tags and context share one 2,000-byte limit.
- Change a record with `machine-memory update <id> "<whole new content>" --remote`. It replaces the content, so read the record first and keep the details that still hold. Leave the content out to change only tags, type or certainty. Create one with `machine-memory add "<content>" --upsert-match "<topic>" --type <type> --remote`, which updates a strong match instead of adding a duplicate.
- When a write reports `dropped` or `review_candidates` (also on stderr, in a `machine-memory:` line), restore details dropped by mistake, and update or deprecate the listed records that rely on what you changed.
- `--certainty verified` requires `--evidence`: `user_requirement`, `observed_implementation`, `verified_mechanism` or `tested_behavior`. Reading the code proves only `observed_implementation`.
- When delegating, scan once and pass the record ids to child agents. Children return findings with their evidence; the parent writes them.
- Before the final response, run `machine-memory checkpoint --remote --json-min`, then add, update or deprecate records for what the task established.

Project preference: replace obsolete systems when practical; preserve backwards compatibility only when it is explicitly required.
<!-- machine-memory:end -->

## Build and test

Rust workspace: `crates/core` (the memory engine), `crates/cli` (the `clanka` binary and the code host plugins share), `crates/py` (Python bindings for the Hermes plugin in `hermes/clanka`).

- Format: `cargo fmt --all`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Test: `cargo test --workspace` (no model provider, keyring, Hermes or network needed)
- Build the CLI: `cargo build -p clanka` (`target/debug/clanka`)

CI (`.github/workflows/ci.yml`) runs on every push to `main` and every pull request: first the advisory check (`.github/workflows/advisories.yml`, `cargo deny check advisories` against `Cargo.lock`), then the format check, lint and tests, which are skipped when the advisory check fails. An advisory with no fix yet needs an `ignore` entry in `deny.toml` before anything else is checked.

To run the CLI without touching real companions or the keyring, point it at a throwaway directory whose `config.toml` holds `[keys]` and `store = "file"`.

On Linux the file is `<dir>/config/clanka/config.toml`:

```sh
env XDG_CONFIG_HOME=<dir>/config XDG_DATA_HOME=<dir>/data DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent ./target/debug/clanka ...
```

On macOS config and data both live in `$HOME/Library/Application Support/clanka`, so the file is `<dir>/Library/Application Support/clanka/config.toml`:

```sh
env HOME=<dir> ./target/debug/clanka ...
```

## Releases

The version is `[workspace.package] version` in `Cargo.toml`, which every crate inherits, and `version` in `hermes/clanka/plugin.yaml`; a test fails when the two differ. To release, bump both (semver: a schema change or a breaking CLI change is at least a minor bump while below 1.0), commit, then tag and push `v<version>`. `.github/workflows/release.yml` checks the tag against both files, builds `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin` (no Windows, no Intel macOS: ort has no prebuilt ONNX Runtime for it), and publishes `clanka-<target>.tar.gz` (the CLI, `hermes/clanka/` with `_clanka.abi3.so`, `skills/soul-writer/SKILL.md`) and `SHA256SUMS`. `hermes/install.sh` and `clanka upgrade` (`crates/cli/src/upgrade.rs`) install the latest release only after its archive matches `SHA256SUMS`, so keep the archive names and layout in step across the three. Tag only when asked.

A schema change is a new file in `crates/core/src/migrations/`, added to `MIGRATIONS` in `crates/core/src/db.rs`; never edit a migration that has shipped.

## Docs site

`docs/` is the Nimbus (Astro) documentation site for `clanka.jfa.dev` and `iac/alchemy.run.ts` is the Alchemy stack that deploys it: one `Cloudflare.Website.StaticSite` that runs `bun run build` in `docs/` and takes the custom domain. They form a Bun workspace (root `package.json`) that the Rust build and CI do not use. `docs/AGENT.md` covers authoring.

- Install: `bun install` (repo root)
- Check: `bun run docs:check`, then `bun run docs:build`; only the build validates the sidebar and produces `docs/dist/pagefind/`
- Plan: `bun run plan` (read-only); deploy: `bun run deploy`. Deploy only when asked.
- CI is Cloudflare Workers Builds on the `clanka-docs` Worker, not GitHub Actions: a push to `main` that touches `docs/`, `package.json`, `bun.lock` or `bunfig.toml` runs `bun run ci:build` and then `bun run ci:deploy`. The trigger sets `ALCHEMY_STAGE=live_jfalava`, the stage the Worker was first deployed from, so builds and local runs share one state; without it a build has no `USER` and would use `live_unknown`.

The CLI pages under `docs/src/content/docs/cli/` mirror `clanka help <command>`, and `configuration.mdx` mirrors the defaults in `crates/core`. Update them with the command or setting they describe.
