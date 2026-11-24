import { describe, expect, test } from "bun:test";
import { Schema } from "effect";

import { Detail, FactIssue, Found, Listed, Memory, Overview, WriteEntry } from "../src/api";
import { current, everything, matching } from "../src/main";
import companions from "./fixtures/companions.json";
import facts from "./fixtures/facts.json";
import log from "./fixtures/log.json";
import memories from "./fixtures/memories.json";
import memory from "./fixtures/memory.json";
import overview from "./fixtures/overview.json";
import recall from "./fixtures/recall.json";

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
