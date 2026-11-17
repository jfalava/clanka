import { expect, test } from "bun:test";
import type { Fetcher } from "@cloudflare/workers-types";
import router, { type Env } from "../src/index";

interface Forwarded {
  path: string;
  method: string;
  authorization: string | null;
  body: string;
}

/** Bindings that note what reached them, and the requests each got. */
function bindings() {
  const calls: Record<keyof Env, Forwarded[]> = { INGEST: [], DOCS: [] };
  const stub = (binding: keyof Env): Fetcher => ({
    fetch: async (input: Request | URL | string, init?: RequestInit) => {
      const request =
        input instanceof Request
          ? input
          : new Request(input instanceof URL ? input.href : input, init);
      const url = new URL(request.url);
      calls[binding].push({
        path: url.pathname + url.search,
        method: request.method,
        authorization: request.headers.get("Authorization"),
        body: await request.text(),
      });
      return new Response(binding, { status: binding === "INGEST" ? 201 : 200 });
    },
    // The router only fetches.
    connect: () => {
      throw new Error(`connect is not stubbed for ${binding}`);
    },
  });
  return { calls, env: { INGEST: stub("INGEST"), DOCS: stub("DOCS") } satisfies Env };
}

test("a send reaches the endpoint without the /ingest prefix, as it was sent", async () => {
  const { calls, env } = bindings();
  const response = await router.request(
    "https://clanka.example/ingest/v1/events?x=1",
    { method: "POST", headers: { Authorization: "Bearer s3cret" }, body: '{"event":"open"}\n' },
    env,
  );
  // The endpoint's own answer comes back.
  expect([response.status, await response.text()]).toEqual([201, "INGEST"]);
  expect(calls.INGEST).toEqual([
    {
      path: "/v1/events?x=1",
      method: "POST",
      authorization: "Bearer s3cret",
      body: '{"event":"open"}\n',
    },
  ]);
  expect(calls.DOCS).toEqual([]);
});

test("/ingest alone is the endpoint's root", async () => {
  const { calls, env } = bindings();
  await router.request("https://clanka.example/ingest", {}, env);
  await router.request("https://clanka.example/ingest/", {}, env);
  expect(calls.INGEST.map((call) => call.path)).toEqual(["/", "/"]);
});

test("everything else goes to the docs untouched", async () => {
  const { calls, env } = bindings();
  // The last two only look like the endpoint's prefix.
  const paths = ["/", "/cli/diagnostics/", "/install.sh", "/ingestion", "/cli/ingest/v1/events"];
  for (const path of paths) {
    const response = await router.request(`https://clanka.example${path}`, {}, env);
    expect(await response.text()).toBe("DOCS");
  }
  expect(calls.DOCS.map((call) => call.path)).toEqual(paths);
  expect(calls.INGEST).toEqual([]);
});
