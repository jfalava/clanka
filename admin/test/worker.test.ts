import { Database } from "bun:sqlite";
import { afterEach, beforeAll, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";

import worker, { type Env } from "../src/worker";

const TEAM = "team.cloudflareaccess.com";
const AUD = "console-aud";
const NOW = Date.now();
const DAY_MS = 24 * 60 * 60 * 1000;
const INSTALL_A = "a".repeat(32);
const INSTALL_B = "b".repeat(32);

// A key pair standing in for the Access team's, published at its certs URL.
let keys: CryptoKeyPair;
let jwk: JsonWebKey & { kid: string; alg: string };

beforeAll(async () => {
  keys = await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
  jwk = { ...(await crypto.subtle.exportKey("jwk", keys.publicKey)), kid: "k1", alg: "RS256" };
});

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");

const sign = async (claims: Record<string, unknown>, signer = keys.privateKey) => {
  const content = `${encode({ alg: "RS256", kid: "k1", typ: "JWT" })}.${encode(claims)}`;
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    signer,
    new TextEncoder().encode(content),
  );
  return `${content}.${Buffer.from(signature).toString("base64url")}`;
};

const valid = () =>
  sign({ aud: [AUD], iss: `https://${TEAM}`, exp: NOW / 1000 + 600, email: "hello@jfa.dev" });

/** SQLite with the endpoint's real schema, behind the part of D1 the console uses. */
function console_() {
  const db = new Database(":memory:");
  db.exec(readFileSync(`${import.meta.dir}/../../ingest/migrations/0001_events.sql`, "utf8"));
  const statement = (sql: string, values: Array<string | number | null>) => ({
    bind: (...bound: Array<string | number | null>) => statement(sql, bound),
    all: async () => ({ results: db.query(sql).all(...values) }),
  });
  const assets = mock(async (_request: Request) => new Response("<html>console</html>"));
  const env = {
    DB: { prepare: (sql: string) => statement(sql, []) },
    ASSETS: { fetch: assets },
    ACCESS_TEAM_DOMAIN: TEAM,
    ACCESS_AUD: AUD,
  } as unknown as Env;

  globalThis.fetch = mock(async (input: RequestInfo | URL) =>
    String(input) === `https://${TEAM}/cdn-cgi/access/certs`
      ? Response.json({ keys: [jwk] })
      : new Response("unexpected fetch", { status: 500 }),
  ) as unknown as typeof fetch;

  let id = 0;
  const add = (fields: {
    sender?: string;
    install?: string;
    at: number;
    event: string;
    error?: string;
    ms?: number;
    pass?: string;
    body?: Record<string, unknown>;
  }) => {
    id += 1;
    db.query(
      `INSERT INTO events (received_at, sender, install, digest, at, event, host, version, os, arch,
                           pass, error, ms, body)
       VALUES (?, ?, ?, ?, ?, ?, 'mcp', '0.1.5', 'macos', 'aarch64', ?, ?, ?, ?)`,
    ).run(
      fields.at,
      fields.sender ?? "jfalava-mac-mini",
      fields.install ?? INSTALL_A,
      String(id).padStart(32, "0"),
      fields.at,
      fields.event,
      fields.pass ?? null,
      fields.error ?? null,
      fields.ms ?? null,
      JSON.stringify({ event: fields.event, ...fields.body }),
    );
  };

  const get = async (path: string, assertion: string | null) => {
    const response = await worker.fetch(
      new Request(`https://clanka.jfa.dev${path}`, {
        headers: assertion === null ? {} : { "Cf-Access-Jwt-Assertion": assertion },
      }) as Parameters<typeof worker.fetch>[0],
      env,
    );
    return { status: response.status, body: (await response.json()) as Record<string, any> };
  };
  return { env, assets, add, get };
}

test("the API answers only requests Access signed for this application", async () => {
  const { get } = console_();
  const other = await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign"],
  );
  const claims = { aud: [AUD], iss: `https://${TEAM}`, exp: NOW / 1000 + 600 };

  expect((await get("/admin/api/overview?days=7", await valid())).status).toBe(200);
  for (const assertion of [
    null,
    "not.a.jwt",
    await sign({ ...claims, aud: ["another-app"] }),
    await sign({ ...claims, iss: "https://other.cloudflareaccess.com" }),
    await sign({ ...claims, exp: NOW / 1000 - 600 }),
    await sign(claims, other.privateKey),
  ]) {
    expect(await get("/admin/api/overview?days=7", assertion)).toEqual({
      status: 403,
      body: { _tag: "Forbidden", error: "Sign in through Cloudflare Access." },
    });
  }
});

test("without an Access application the API refuses everything", async () => {
  const { env, get } = console_();
  env.ACCESS_AUD = "";
  expect((await get("/admin/api/overview?days=7", await valid())).status).toBe(503);
});

test("the page's files come from ASSETS; the API only reads", async () => {
  const { assets, env } = console_();
  const response = await worker.fetch(
    new Request("https://clanka.jfa.dev/admin/") as Parameters<typeof worker.fetch>[0],
    env,
  );
  expect(await response.text()).toBe("<html>console</html>");
  expect(assets).toHaveBeenCalledTimes(1);

  const post = await worker.fetch(
    new Request("https://clanka.jfa.dev/admin/api/events?days=7", {
      method: "POST",
      headers: { "Cf-Access-Jwt-Assertion": await valid() },
    }) as Parameters<typeof worker.fetch>[0],
    env,
  );
  expect(post.status).toBe(405);
});

test("the overview counts the period and filters it asks for", async () => {
  const { add, get } = console_();
  add({ at: NOW - 1000, event: "memory_pass", pass: "reflect", ms: 100 });
  add({ at: NOW - 2000, event: "memory_pass", pass: "reflect", ms: 300, error: "llm" });
  add({ at: NOW - 3000, event: "plugin", body: { what: "turn_blocked" }, install: INSTALL_B, sender: "jfalava-oci-box" });
  add({ at: NOW - 3 * DAY_MS, event: "command", body: { name: "doctor" }, ms: 40 });
  // Older than the longest period asked for below: left out.
  add({ at: NOW - 10 * DAY_MS, event: "command", body: { name: "doctor" }, error: "io" });

  const week = (await get("/admin/api/overview?days=7", await valid())).body;
  expect(week.totals).toMatchObject({ events: 4, errors: 1, installs: 2, senders: 2 });
  expect(week.byEvent).toEqual([
    { event: "memory_pass", detail: "reflect", count: 2, errors: 1, avgMs: 200 },
    { event: "command", detail: "doctor", count: 1, errors: 0, avgMs: 40 },
    { event: "plugin", detail: "turn_blocked", count: 1, errors: 0, avgMs: null },
  ]);
  expect(week.byError).toEqual([
    { event: "memory_pass", error: "llm", count: 1, lastAt: NOW - 2000 },
  ]);
  expect(week.installs.map((row: { install: string; events: number }) => [row.install, row.events])).toEqual([
    [INSTALL_A, 3],
    [INSTALL_B, 1],
  ]);
  // The menus list every sender and kind kept, whatever the period.
  expect(week.senders).toEqual(["jfalava-mac-mini", "jfalava-oci-box"]);
  expect(week.events).toEqual(["command", "memory_pass", "plugin"]);

  const day = (await get("/admin/api/overview?days=1&sender=jfalava-mac-mini&errors=only", await valid())).body;
  expect(day.totals).toMatchObject({ events: 1, errors: 1, installs: 1, senders: 1 });
});

test("events come newest first, a page at a time", async () => {
  const { add, get } = console_();
  // Received oldest last, as a batch sent late would be; two share a time.
  for (let index = 0; index < 55; index += 1) {
    add({ at: NOW - Math.min(index, 53) * 1000, event: "recall" });
  }
  const first = (await get("/admin/api/events?days=1", await valid())).body;
  expect(first.events).toHaveLength(50);
  expect(first.events[0].at).toBe(NOW);
  expect(JSON.parse(first.events[0].body)).toEqual({ event: "recall" });

  const second = (await get(`/admin/api/events?days=1&before=${first.next}`, await valid())).body;
  expect(second.events).toHaveLength(5);
  expect(second.next).toBeNull();
  const seen = [...first.events, ...second.events].map((event: { id: number }) => event.id);
  expect(new Set(seen).size).toBe(55);
  expect(second.events.at(-1).at).toBe(NOW - 53 * 1000);
});
