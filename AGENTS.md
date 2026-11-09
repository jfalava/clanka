<!-- machine-memory:start -->
## Project memory

This project uses `machine-memory` with a shared remote Worker-backed database.
Every database-backed command requires exactly one backend flag: use `--remote` for this repository.
Run `machine-memory doctor` during maintenance, not every task. Do not create or rely on a local `machine-memory.db` for this repository.
Search policy: use `machine-memory query "<term>" --remote` for exact names, paths, commands, and identifiers; use `--semantic --remote` when the same concept may use different wording; use `--hybrid --remote` for broad investigation or when recall matters more than exact matching.
When unsure, start with `--hybrid`; add `--explain-score` when ranking needs inspection. D1 records are canonical and Vectorize is only a retrieval index.
To read or write another repository's memories, pass `--repo owner/name` instead of changing directory; `add` and `import` refuse a repository with no memories unless `--allow-new-repo` is given. `machine-memory <command> --help` prints help for that command only.
After adding or updating a memory, its Vectorize embedding is synchronized automatically. Run `machine-memory reindex --remote` only after provisioning, changing the embedding index or model, or repairing missing vectors.
Memory size: the composed embedding text must stay below 512 BGE tokens AND within the embedding service's conservative byte+2 estimate (512 bytes). Preflight without writing: `machine-memory size "<text>"` or add/update `--dry-run` (both exit 1 when over budget); failures name the exact byte or token deficit. `--token-report` appends the per-part breakdown to real writes.

⚠️ MANDATORY: Complete the memory scan BEFORE any code changes. Skipping it causes rework, regressions, and duplicated decisions.

### Required pre-workflow (DO NOT SKIP)

Before touching code, complete this scan from the repository root. Every database command must include the backend flag shown below:

- Known files: `machine-memory suggest --files "path/a.ts,path/b.ts" --remote --json-min`
- Known topic: `machine-memory query "topic" --remote --json-min`
- Broad audit: `machine-memory list --tags "area:..." --remote --json-min`

If results look relevant, fetch full records before editing: `machine-memory get <id> --remote` or `machine-memory get <id,id,...> --remote`.

Search results are ranked candidates, not an exhaustive or authoritative list. Scores only rank results within one response, so a high score does not prove a strong match. Use each result's `match_strength` (strong, weak, none) and `exact_matches` instead; when the response reports `no_strong_match`, nothing matched well. Prefer known IDs (`get`), file-scoped `suggest --files`, and `--tags` filters over broad queries, read full records before relying on them, and treat "nothing directly relevant" as a valid outcome instead of adopting the nearest hit.

Recover accidental overwrites with `machine-memory history <id> --remote`, then `machine-memory restore <id> --revision <revision_id> --expected-update-count <current_update_count> --remote`. Use the current count from history, not the snapshot's count. Restore preserves the id and re-syncs only the current memory's vector; snapshots are not embedded. Delete is permanent and removes history.

### One-sweep workflow (use this every task)

1. Scan relevant context fast. Run exactly one focused `suggest`, `query`, or `list` command before code changes; repeat only if the touched paths or scope materially changes.
2. Verify uncertain context before acting. Use `machine-memory verify <id> "<inferred fact>" --remote` or `machine-memory diff <id> "<proposed updated wording>" --remote` when an inference may conflict with existing memory.
3. Maintain memory while implementing. Prefer `machine-memory update --match "topic query" "new canonical content" --remote`; if no reliable match exists, use `machine-memory add "..." --upsert-match "topic query" --remote`. Weak matches refuse to silently create: inspect with `--dry-run`, then pass `--force` if a new record is intended. An update replaces the whole content: read the current record first and carry over unrelated valid details such as source paths, versions, and caveats. `update --dry-run` and `diff` report `dropped` identifiers and sentences the new text would lose; keep each one or confirm it is obsolete.
4. Reconcile dependents. When a decision changes or is reversed, update or deprecate the other active records that rely on the old premise. Updates and deprecations return `review_candidates` (records sharing the old premise; strong ones name the same path or identifier): resolve each one, or confirm it still holds. Consolidate overlapping records instead of adding a partially redundant one; isolate a version-specific limitation in its own record and reference the existing workaround rather than repeating it. Use `machine-memory deprecate <id> --superseded-by <id> --remote` when a record is replaced.
5. Write for retrieval. Put commands, API paths, file paths, keys, routes, thresholds, and exact feature keywords in the first sentence.
6. Use path-driven tags. Prefer `--path` and `tag-map`; use scoped tags such as `area:cli,topic:backend,kind:decision` when no mapping exists.
7. Capture third-party quirks. Always add a `--type gotcha` memory for surprising library or tool behavior, leading with the library name, version, behavior, and fix.
8. Set certainty from evidence, not from how accurately a record describes the code. Record the kinds of evidence behind each claim: `user_requirement` (an explicit user requirement or preference), `observed_implementation` (what the code currently does), `verified_mechanism` (a confirmed technical mechanism), `tested_behavior` (end-user behavior exercised in tests or real use). Observing that the code does X establishes only `observed_implementation`, not that X is intended or works on every platform. `verified` is rejected without at least one evidence kind. Record remaining limits (unconfirmed intent, untested platforms) as validation gaps, and keep sources and versions in refs or context. Use `--certainty`, `--evidence <kinds>` (comma-separated), `--validation-gaps <text|json_array>`, and `--refs` or `--context`. `doctor` lists legacy `verified` records without evidence as `verified_without_evidence`; review them instead of bulk-changing them.
9. Separate durable knowledge from delivery status. Durable mechanisms and decisions go in `decision`, `reference`, `gotcha`, `constraint`, or `preference` records. Implementation, delivery, and verification state goes in one `status` record that expires and is updated rather than duplicated. State exactly where work stands (local only, uncommitted, committed, pushed, deployed or active) and which verification remains; never describe local work as deployed. When a caveat leaves a durable record, keep it in a status record. Set `--expires-after-days` on status records and review `doctor` suggestions semantically before applying deprecations or updates.
10. Run the end-of-task checkpoint below before the final response.

### Delegated work (parent and child agents)

- The parent agent owns memory. It runs the scan once and passes relevant memory IDs and constraints to child agents; children skip the mandatory scan unless their scope differs, and then use focused `suggest` or `query` calls.
- Children do not write memory unless the parent explicitly delegates writes to exactly one child. Otherwise they return persistence candidates: the finding, its evidence (file paths, commands, versions, test output), the evidence kind, and related memory IDs.
- The parent reconciles its own findings and every child candidate in the end-of-task checkpoint.

### End-of-task checkpoint (before the final response)

Start with `machine-memory checkpoint --remote --json-min`: it lists memories written since `--since` (default 12 hours), memories related to the touched files (default: the Git working-tree changes, else the last commit; override with `--files`), review flags such as `verified_without_evidence` and `status_without_expiry`, and the commands for each group below.

Collect every validated finding from the task, including child results and mechanisms discovered while investigating approaches that were later abandoned. A rejected remedy does not make the verified mechanism behind its failure disposable. Classify each finding:

1. Already covered: cite the memory ID; change it with `machine-memory update ... --remote` only when evidence, scope, or wording is wrong.
2. New durable knowledge: create a `decision`, `constraint`, `preference`, `reference`, or `gotcha` record with `machine-memory add ... --remote`.
3. Superseded or conflicting: fix the affected records with `machine-memory update ... --remote` or `machine-memory deprecate ... --remote`, including records that depend on the old premise.
4. Transient delivery or verification status: add or update one expiring `status` record.
5. Intentionally omitted: have a reason, such as obvious from code, routine test result, duplicate, or not reusable.

Saved records keep their source and version evidence and their verification boundaries.

### Checklist (verify before proceeding)

- [ ] I ran `machine-memory suggest`, `query`, or `list` with --remote for the files or feature I will touch (or reused the parent agent's scan)
- [ ] I reviewed the returned memory IDs and fetched full records when relevant, without treating top results as exhaustive
- [ ] I considered whether existing memories constrain the planned approach
- [ ] I will run the end-of-task checkpoint, including child findings, before the final response

Project preference: replace obsolete systems when practical; preserve backwards compatibility only when it is explicitly required.
<!-- machine-memory:end -->
