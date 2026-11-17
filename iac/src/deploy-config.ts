// What a deployment of this stack is: the names of its Workers and database,
// its hostname, whether it serves the docs, and the Cloudflare Access
// application in front of the telemetry console. Everything lives in the
// deploying account, so a clone in another account deploys its own copy; a
// second deployment in the same account needs a `name` of its own, or it takes
// over the first one's Workers and database.
//
// The file is CLANKA_DEPLOY_CONFIG when set, else clanka.deploy.json in the
// directory Alchemy runs from (iac/, gitignored), else none: the endpoint on
// the router's workers.dev URL, without the docs or the console. This
// repository's own deployment is clanka.production.deploy.json, which the
// package scripts select.
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

import { Schema } from "effect";

const Access = Schema.Struct({
  /** The Access team's domain, `<team>.cloudflareaccess.com`. */
  teamDomain: Schema.String.check(Schema.isPattern(/^[a-z0-9-]+\.cloudflareaccess\.com$/)),
  /** The audience tag of the Access application on `<domain>/admin`. */
  aud: Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/)),
});

const DeployFile = Schema.Struct({
  /** What every Worker and the database are named after: `<name>-router` and so on. */
  name: Schema.optional(Schema.String.check(Schema.isPattern(/^[a-z][a-z0-9-]{0,30}$/))),
  /** The router's hostname, in a zone of the account. None: its workers.dev URL. */
  domain: Schema.optional(Schema.String.check(Schema.isMinLength(1))),
  /** Serve the docs site (docs/) at the root. clanka.jfa.dev's; off by default. */
  docs: Schema.optional(Schema.Boolean),
  /** Deploy the telemetry console at /admin, behind this Access application. */
  access: Schema.optional(Access),
});

export interface DeployConfig {
  readonly name: string;
  readonly domain: string | undefined;
  readonly docs: boolean;
  readonly access: typeof Access.Type | undefined;
  /** The file read, or undefined when there was none. */
  readonly path: string | undefined;
}

const DEFAULT_NAME = "clanka";

const decode = Schema.decodeUnknownSync(DeployFile, { onExcessProperty: "error" });

export function loadDeployConfig(
  env: Record<string, string | undefined> = process.env,
  cwd: string = process.cwd(),
): DeployConfig {
  const chosen = env.CLANKA_DEPLOY_CONFIG?.trim();
  const local = resolve(cwd, "clanka.deploy.json");
  const path = chosen ? resolve(cwd, chosen) : existsSync(local) ? local : undefined;
  if (path === undefined) {
    return { name: DEFAULT_NAME, domain: undefined, docs: false, access: undefined, path };
  }
  if (!existsSync(path)) {
    throw new Error(`CLANKA_DEPLOY_CONFIG names ${path}, which does not exist.`);
  }
  let file: typeof DeployFile.Type;
  try {
    file = decode(JSON.parse(readFileSync(path, "utf8")));
  } catch (cause) {
    throw new Error(`${path} is not a deploy config: ${String(cause)}`, { cause });
  }
  return {
    name: file.name ?? DEFAULT_NAME,
    domain: file.domain,
    docs: file.docs ?? false,
    access: file.access,
    path,
  };
}
