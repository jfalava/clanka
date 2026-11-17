import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { loadDeployConfig } from "../src/deploy-config";

const AUD = "ab".repeat(32);
const made: Array<string> = [];

afterEach(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true });
});

/** A directory to run from, holding these files. */
const directory = (files: Record<string, unknown> = {}) => {
  const dir = mkdtempSync(join(tmpdir(), "clanka-deploy-"));
  made.push(dir);
  for (const [name, content] of Object.entries(files)) {
    writeFileSync(join(dir, name), typeof content === "string" ? content : JSON.stringify(content));
  }
  return dir;
};

test("with no config a clone deploys the endpoint alone, on its workers.dev URL", () => {
  expect(loadDeployConfig({}, directory())).toEqual({
    name: "clanka",
    domain: undefined,
    docs: false,
    access: undefined,
    path: undefined,
  });
});

test("clanka.deploy.json is found where Alchemy runs; CLANKA_DEPLOY_CONFIG wins over it", () => {
  const dir = directory({
    "clanka.deploy.json": { domain: "clanka.example.com" },
    "other.json": {
      name: "clanka-staging",
      docs: true,
      access: { teamDomain: "team.cloudflareaccess.com", aud: AUD },
    },
  });
  expect(loadDeployConfig({}, dir)).toMatchObject({ domain: "clanka.example.com", docs: false });
  expect(loadDeployConfig({ CLANKA_DEPLOY_CONFIG: "./other.json" }, dir)).toEqual({
    name: "clanka-staging",
    domain: undefined,
    docs: true,
    access: { teamDomain: "team.cloudflareaccess.com", aud: AUD },
    path: join(dir, "other.json"),
  });
});

test("this repository's production config is clanka.jfa.dev with everything", () => {
  const config = loadDeployConfig(
    { CLANKA_DEPLOY_CONFIG: "clanka.production.deploy.json" },
    `${import.meta.dir}/..`,
  );
  expect(config.name).toBe("clanka");
  expect(config.domain).toBe("clanka.jfa.dev");
  expect(config.docs).toBe(true);
  expect(config.access?.teamDomain).toBe("jfa-d.cloudflareaccess.com");
});

test("a named file that is missing, a typo or a malformed value is an error, not a default", () => {
  const dir = directory({
    "typo.json": { domian: "clanka.example.com" },
    "name.json": { name: "Clanka Staging" },
    "team.json": { access: { teamDomain: "https://team.cloudflareaccess.com", aud: AUD } },
    "broken.json": "{",
  });
  for (const name of ["missing.json", "typo.json", "name.json", "team.json", "broken.json"]) {
    expect(() => loadDeployConfig({ CLANKA_DEPLOY_CONFIG: name }, dir)).toThrow(name);
  }
});
