# clanka's Cloudflare stack

`alchemy.run.ts` deploys, into one Cloudflare account:

- `clanka-ingest` (`ingest/`): the diagnostics endpoint `clanka diagnostics send` posts to, and
  the `clanka-diagnostics` D1 database it keeps events in for 90 days.
- `clanka-router` (`router/`): the only Worker with a hostname. `/ingest` goes to the endpoint,
  `/admin` to the console, everything else to the docs.
- `clanka-admin` (`admin/`), optional: a read-only telemetry console behind Cloudflare Access.
- `clanka-docs` (`docs/`), optional: the documentation site, clanka.jfa.dev's.

## Configuration

A deploy config says which parts a deployment has. Alchemy reads the file named by
`CLANKA_DEPLOY_CONFIG`, else `clanka.deploy.json` in the directory it runs from (`iac/`,
gitignored), else none. A named file that is missing or has an unknown key is an error.

| Key                 | Without it                                       |
| ------------------- | ------------------------------------------------ |
| `name`              | `clanka`: `clanka-router`, `clanka-diagnostics`… |
| `domain`            | The router answers on its `workers.dev` URL.     |
| `docs`              | `false`: no docs; the root answers 404.          |
| `access.teamDomain` | No console; `/admin` answers 404.                |
| `access.aud`        | (Both `access` fields go together.)              |

Everything lives in the account that deploys it, so a deployment in another account does not
touch this repository's. A second deployment in the same account needs its own `name`: with the
same one it takes over the first one's Workers and database.

`bun run deploy`, `plan`, `destroy` and `ci:deploy` select `clanka.production.deploy.json`, which
is clanka.jfa.dev with everything. Run Alchemy directly for any other deployment.

## Deploying your own

1. `bun install` at the repository root, then connect your Cloudflare account with
   `bunx alchemy profile edit` in `iac/`. It needs to edit Workers, D1 and, with a domain, DNS.
2. Copy `clanka.deploy.example.json` to `iac/clanka.deploy.json`. Set `domain` to a hostname in a
   zone of your account, or remove it to use the `workers.dev` URL.
3. For the console, add a Cloudflare Access self-hosted application on `<domain>/admin` (Zero
   Trust → Access → Applications) and put its team domain and Application Audience (AUD) tag in
   `access`. Leave `access` out to skip the console.
4. In `iac/`: `bunx alchemy plan`, then `bunx alchemy deploy`. The output's `url` is the router.

Point a clanka install at it with `url = "<url>/ingest/v1/events"` under
`[diagnostics.upload]`. Each sender makes a token with `clanka diagnostics token --new` and gives
you the hash it prints; let it in on the `<name>-diagnostics` database:

```sh
bunx wrangler d1 execute <name>-diagnostics --remote --command \
  "INSERT INTO tokens (hash, label, created_at) VALUES ('<hash>', '<label>', unixepoch() * 1000)"
```

Revoke it with `UPDATE tokens SET revoked_at = unixepoch() * 1000 WHERE label = '<label>'`.

The code is AGPL-3.0: if you change the Workers and run them for others, offer those users the
changed source.
