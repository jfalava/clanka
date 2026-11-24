import { describe, expect, test } from "bun:test";
import { Schema } from "effect";

import {
  Deletion,
  Detail,
  Ended,
  FactIssue,
  Found,
  Frozen,
  Listed,
  Memory,
  Overview,
  REREAD_BATCH,
  Reread,
  Rollback,
  RollbackPlan,
  WriteEntry,
} from "../src/api";
import {
  Message,
  current,
  everything,
  init,
  isConfirmable,
  matching,
  rereadable,
  update,
  type Model,
} from "../src/main";
import companions from "./fixtures/companions.json";
import deletion from "./fixtures/deletion.json";
import ended from "./fixtures/ended.json";
import facts from "./fixtures/facts.json";
import frozen from "./fixtures/frozen.json";
import log from "./fixtures/log.json";
import memories from "./fixtures/memories.json";
import memory from "./fixtures/memory.json";
import overview from "./fixtures/overview.json";
import recall from "./fixtures/recall.json";
import reread from "./fixtures/reread.json";
import rollback from "./fixtures/rollback.json";
import rollbackPlan from "./fixtures/rollback_plan.json";

// The fixtures are what `clanka web` sends for a seeded companion: its own test
// (crates/cli/src/web/tests.rs) fails when they stop being that. Decoding them here, with a field
// the page does not know counted as an error, keeps src/api.ts the server's shape in both
// directions.
describe("what the server sends decodes as what the page expects", () => {
  const decodes = <A>(schema: Schema.Codec<A, unknown>, sent: unknown) =>
    Schema.decodeUnknownSync(schema, { onExcessProperty: "error" })(sent);

  test("the companions", () => {
    expect(decodes(Schema.Array(Listed), companions)).toHaveLength(1);
  });
  test("a companion", () => {
    expect(decodes(Overview, overview).mood?.cause).toBe("Marisol left");
  });
  test("its memories, with every optional field filled once", () => {
    const all = decodes(Schema.Array(Memory), memories);
    expect(all.some(({ superseded_by }) => superseded_by !== null)).toBe(true);
    expect(all.some(({ resolved_at }) => resolved_at !== null)).toBe(true);
    expect(all.some(({ strength }) => strength !== null)).toBe(true);
    expect(all.some(({ urgency }) => urgency !== null)).toBe(true);
    expect(all.some(({ valence }) => valence !== null)).toBe(true);
    expect(all.some(({ source_turns }) => source_turns.length > 0)).toBe(true);
  });
  test("one memory's history and turns", () => {
    const detail = decodes(Detail, memory);
    expect(detail.versions.length).toBeGreaterThan(1);
    expect(detail.turns.length).toBeGreaterThan(0);
  });
  test("the facts never said", () => {
    expect(decodes(Schema.Array(FactIssue), facts)[0]?.fact).toEqual(["city", "Oslo"]);
  });
  test("what a query brings up", () => {
    expect(decodes(Found, recall).found.length).toBeGreaterThan(0);
  });
  test("the write log", () => {
    expect(decodes(Schema.Array(WriteEntry), log).length).toBeGreaterThan(0);
  });
  test("a companion on a run that repairs", () => {
    const sent = decodes(Overview, overview);
    expect([sent.can_repair, sent.model]).toEqual([true, "a-model"]);
  });
  test("what a delete would remove", () => {
    expect(decodes(Deletion, deletion).restored.length).toBeGreaterThan(0);
  });
  test("a rollback's plan, and the rollback that then says the same", () => {
    const planned = decodes(RollbackPlan, rollbackPlan);
    expect(planned.plan.removed.length).toBeGreaterThan(0);
    expect(decodes(Rollback, rollback)).toEqual(planned.plan);
  });
  test("memories re-read, with a call the companion was refused", () => {
    const sent = decodes(Reread, reread);
    expect([sent.rewritten, sent.refused.length]).toEqual([1, 1]);
  });
  test("a freeze and an ended conversation", () => {
    expect(decodes(Frozen, frozen).frozen).toBe(true);
    expect(decodes(Ended, ended).ended).toBe("conversation");
  });
});

describe("the doctor's repairs", () => {
  const all = Schema.decodeUnknownSync(Schema.Array(Memory))(memories);
  const entries = Schema.decodeUnknownSync(Schema.Array(WriteEntry))(log);
  const plan = Schema.decodeUnknownSync(RollbackPlan)(rollbackPlan);
  const companion = Schema.decodeUnknownSync(Overview)(overview);

  /** The page with the fixtures' companion chosen and read. */
  const loaded = (): Model => {
    const listed = update(
      init({ theme: "System", token: "t", isCompanionsOpen: true }).model,
      Message.SucceededFetchCompanions({
        companions: Schema.decodeUnknownSync(Schema.Array(Listed))(companions),
      }),
    ).model;
    return update(
      listed,
      Message.SucceededFetchCompanion({
        generation: listed.generation,
        loaded: { overview: companion, memories: all, issues: [] },
      }),
    ).model;
  };
  /** One message after another; the names of the commands the last one started. */
  const after = (model: Model, messages: ReadonlyArray<Message>) => {
    let commands: ReadonlyArray<{ name: string; args?: unknown }> = [];
    for (const message of messages) {
      const next = update(model, message);
      model = next.model;
      commands = next.commands ?? [];
    }
    return { model, commands, names: commands.map(({ name }) => name) };
  };

  test("a seared memory and one that cites no turns are not sent to be re-read", () => {
    const [first, second] = all.filter(({ source_turns }) => source_turns.length > 0);
    const uncited = all.find(({ source_turns }) => source_turns.length === 0)!;
    const seared = { ...first!, level: "seared" as const };
    const others = all.filter(({ id }) => id !== first!.id);
    expect(rereadable([seared, ...others], [first!.id, second!.id, uncited.id])).toEqual({
      memories: [second!.id],
      seared: [first!.id],
      uncited: [uncited.id],
    });
    // A seared memory with no turns is listed once, as seared.
    const both = { ...uncited, level: "seared" as const };
    expect(rereadable([both], [uncited.id])).toEqual({
      memories: [],
      seared: [uncited.id],
      uncited: [],
    });
  });

  test("a rollback runs only on the plan that was shown, once the name is typed", () => {
    const entry = entries.find(({ action }) => action === "set_level")!;
    const asked = after(loaded(), [Message.ClickedRollback({ entry })]);
    expect(asked.names).toContain("FetchRollbackPlan");
    expect(asked.commands.find(({ name }) => name === "FetchRollbackPlan")?.args).toMatchObject({
      to: entry.created_at,
      entry: entry.id,
    });
    const generation = asked.model.generation;
    const typed = Message.ChangedTypedName({ value: ` ${companion.name} ` });
    const arrived = Message.SucceededFetchRollbackPlan({ generation, entry: entry.id, plan });
    const rollBacks = (messages: ReadonlyArray<Message>) =>
      after(asked.model, [...messages, Message.ClickedConfirm()]).commands.filter(
        ({ name }) => name === "RollBack",
      );

    expect(rollBacks([])).toEqual([]);
    expect(rollBacks([typed])).toEqual([]);
    expect(rollBacks([arrived])).toEqual([]);
    expect(rollBacks([arrived, Message.ChangedTypedName({ value: "nova" })])).toEqual([]);
    // A plan for another write, or for the companion read before this one, is not this one's.
    const other = Message.SucceededFetchRollbackPlan({ generation, entry: entry.id + 1, plan });
    const stale = Message.SucceededFetchRollbackPlan({
      generation: generation - 1,
      entry: entry.id,
      plan,
    });
    expect(rollBacks([other, typed])).toEqual([]);
    expect(rollBacks([stale, typed])).toEqual([]);

    const ready = after(asked.model, [arrived, typed]);
    expect(isConfirmable(ready.model)).toBe(true);
    const confirmed = after(ready.model, [Message.ClickedConfirm()]);
    expect(confirmed.commands.find(({ name }) => name === "RollBack")?.args).toMatchObject({
      to: entry.created_at,
      seen: plan.seen,
    });
    // A second click while it runs starts nothing more.
    expect(after(confirmed.model, [Message.ClickedConfirm()]).names).not.toContain("RollBack");
  });

  test("a delete waits for its plan, and sends the memories that were asked about", () => {
    const doomed = [all[0]!.id, all[1]!.id];
    const asked = after(loaded(), [Message.ClickedDelete({ memories: doomed })]);
    expect(asked.names).toContain("FetchDeletePlan");
    expect(after(asked.model, [Message.ClickedConfirm()]).names).not.toContain("DeleteMemories");
    const confirmed = after(asked.model, [
      Message.SucceededFetchDeletePlan({
        generation: asked.model.generation,
        memories: doomed,
        plan: { turns: [], restored: [] },
      }),
      // Ticked after the question was put: not part of what was agreed to.
      Message.ToggledMemory({ id: all[2]!.id }),
      Message.ClickedConfirm(),
    ]);
    expect(confirmed.commands.find(({ name }) => name === "DeleteMemories")?.args).toMatchObject({
      memories: doomed,
    });
  });

  test("memories are re-read a batch at a time, and the batches are added up", () => {
    const many = Array.from({ length: 2 * REREAD_BATCH + 5 }, (_, index) => index + 1);
    const sent = (commands: ReadonlyArray<{ name: string; args?: unknown }>) =>
      commands
        .filter(({ name }) => name === "RereadBatch")
        .map(({ args }) => (args as { memories: ReadonlyArray<number> }).memories);
    const answered = (rewritten: number, refused: ReadonlyArray<string> = []) =>
      Message.SucceededRereadBatch({
        companion: companion.id,
        reread: { rewritten, model_calls: 1, refused },
      });

    const first = after(loaded(), [Message.ClickedReread({ memories: many }), Message.ClickedConfirm()]);
    expect(sent(first.commands)).toEqual([many.slice(0, REREAD_BATCH)]);
    const second = after(first.model, [answered(REREAD_BATCH)]);
    expect(sent(second.commands)).toEqual([many.slice(REREAD_BATCH, 2 * REREAD_BATCH)]);
    const third = after(second.model, [answered(REREAD_BATCH - 1, ["reinterpret: error: no"])]);
    expect(sent(third.commands)).toEqual([many.slice(2 * REREAD_BATCH)]);
    const done = after(third.model, [answered(5)]);
    expect(sent(done.commands)).toEqual([]);
    expect(done.model.repair).toMatchObject({
      _tag: "Done",
      summary: `Re-read ${2 * REREAD_BATCH + 4} of ${many.length} memories.`,
      notes: ["reinterpret: error: no"],
    });

    // A batch that fails stops the rest, and says how far it got.
    const failed = after(second.model, [Message.FailedRepair({ error: "llm: no key" })]);
    expect(sent(failed.commands)).toEqual([]);
    expect(failed.model.repair).toMatchObject({ _tag: "Failed" });
    expect((failed.model.repair as { error: string }).error).toContain(
      `${REREAD_BATCH} of ${many.length}`,
    );
  });
});

describe("the memories table", () => {
  const all = Schema.decodeUnknownSync(Schema.Array(Memory))(memories);
  const issues = Schema.decodeUnknownSync(Schema.Array(FactIssue))(facts);
  const ids = (filters: Parameters<typeof matching>[2]) =>
    matching(all, issues, filters).map(({ id }) => id);
  const replaced = all.filter(({ status }) => status === "superseded").map(({ id }) => id);

  test("opens on what the companion holds now, newest first", () => {
    expect(replaced).toHaveLength(1);
    const shown = ids(current);
    expect(shown).toHaveLength(all.length - 1);
    expect(shown).not.toContain(replaced[0]);
    expect(shown).toEqual(shown.toSorted((a, b) => b - a));
    expect(ids(everything)).toHaveLength(all.length);
    expect(ids({ ...everything, status: "superseded" })).toEqual(replaced);
  });

  test("finds words in the text and in the facts, in any case", () => {
    // "oslo" is in three memories' text; "marisol" is in one text and that memory's facts.
    expect(ids({ ...everything, text: "OSLO" })).toHaveLength(3);
    const sister = all.find(({ facts }) => facts["sister"] === "Marisol");
    expect(ids({ ...everything, text: "  marisol " })).toEqual([sister!.id]);
    // A fact's name counts too: only that memory has a "city".
    expect(ids({ ...everything, text: "city" })).toEqual([sister!.id]);
    expect(ids({ ...everything, text: "nothing says this" })).toEqual([]);
  });

  test("#n is the memory with that number, not every text holding the digit", () => {
    expect(ids({ ...everything, text: "#2" })).toEqual([2]);
    expect(ids({ ...everything, text: "#404" })).toEqual([]);
    // Under the default filters a replaced memory stays hidden even by number.
    expect(ids({ ...current, text: `#${replaced[0]}` })).toEqual([]);
  });

  test("narrows by type, level and facts never said, together", () => {
    expect(ids({ ...everything, kind: "opinion" })).toHaveLength(2);
    expect(ids({ ...everything, kind: "opinion", level: "vivid" })).toHaveLength(1);
    expect(ids({ ...everything, facts: "unsaid" })).toEqual(issues.map(({ memory }) => memory));
    expect(ids({ ...everything, facts: "unsaid", kind: "opinion" })).toEqual([]);
  });
});
