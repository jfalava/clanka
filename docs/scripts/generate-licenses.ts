/**
 * Write src/lib/licenses.ts: every direct dependency of the Rust workspace
 * (from `cargo metadata`) and of the Bun workspaces (docs, admin, iac, ingest,
 * router), at its locked or installed version, with its declared license, a
 * short excerpt of its license file, and a link to the full text.
 *
 *   bun scripts/generate-licenses.ts           # regenerate
 *   bun scripts/generate-licenses.ts --check   # fail when the file is stale
 *
 * Optional npm dependencies are skipped: they are per-platform binaries
 * (e.g. @bruits/satteri-*) and only one of them is ever installed.
 */
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const destination = join(root, "docs/src/lib/licenses.ts");
const licenseFilePattern = /^(licen[cs]e|copying)([-.][\w.-]+)?$/i;
const fallbackLicenseFiles = ["LICENSE", "LICENSE.md", "LICENSE.txt", "license", "license.md"];

interface License {
  name: string;
  version: string;
  license: string;
  licenseUrl: string;
  excerpt: string;
}

type Json = Record<string, unknown>;

const isObject = (value: unknown): value is Json =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const text = (value: unknown): string | undefined => (typeof value === "string" ? value : undefined);

async function readJson(path: string): Promise<Json> {
  const value: unknown = JSON.parse(await readFile(path, "utf8"));
  if (!isObject(value)) throw new Error(`${path} is not a JSON object.`);
  return value;
}

/** The first license file in a package directory and its first five lines. */
async function licenseFile(directory: string): Promise<{ file?: string; excerpt: string }> {
  const file = (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && licenseFilePattern.test(entry.name))
    .map((entry) => entry.name)
    .toSorted()[0];
  if (file === undefined) return { excerpt: "" };
  const excerpt = (await readFile(join(directory, file), "utf8"))
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .slice(0, 5)
    .join("\n");
  return { file, excerpt };
}

function githubUrl(source: string | undefined): string {
  return (source ?? "")
    .replace(/^git\+/, "")
    .replace(/^git:\/\//, "https://")
    .replace(/^git@github\.com:/, "https://github.com/")
    .replace(/^github:/, "https://github.com/")
    .replace(/\.git$/, "")
    .replace(/#.*$/, "")
    .replace(/\/(tree|blob)\/.*$/, "")
    .replace(/\/$/, "");
}

/** A GitHub blob link to the license if one is found, otherwise `fallback`. */
async function githubLicense(
  repo: string,
  directory: string | undefined,
  file: string | undefined,
  fallback: string,
): Promise<string> {
  const match = /^https:\/\/github\.com\/([^/]+)\/([^/]+)$/.exec(repo);
  if (match === null) return fallback;
  const files = [...new Set([...(file === undefined ? [] : [file]), ...fallbackLicenseFiles])];
  const paths = [
    ...new Set([...(directory ? files.map((name) => `${directory}/${name}`) : []), ...files]),
  ];
  for (const branch of ["main", "master"]) {
    const found = await Promise.all(
      paths.map(async (path) => {
        try {
          const response = await fetch(
            `https://raw.githubusercontent.com/${match[1]}/${match[2]}/${branch}/${path}`,
            { method: "HEAD", signal: AbortSignal.timeout(5000) },
          );
          return response.ok ? path : undefined;
        } catch {
          return undefined;
        }
      }),
    );
    const path = found.find((candidate) => candidate !== undefined);
    if (path !== undefined) return `${repo}/blob/${branch}/${path}`;
  }
  return fallback;
}

const sorted = (list: License[]): License[] =>
  [...new Map(list.map((item) => [`${item.name}@${item.version}`, item])).values()].toSorted(
    (a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version),
  );

/** Direct dependencies of the Rust workspace's members, of every kind. */
async function crates(): Promise<License[]> {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
    }),
  ) as {
    packages: { id: string; name: string; version: string; license?: string; repository?: string; manifest_path: string }[];
    workspace_members: string[];
    resolve: { nodes: { id: string; deps: { pkg: string }[] }[] };
  };
  const members = new Set(metadata.workspace_members);
  const byId = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const direct = new Set(
    metadata.resolve.nodes
      .filter((node) => members.has(node.id))
      .flatMap((node) => node.deps.map((dep) => dep.pkg))
      .filter((id) => !members.has(id)),
  );
  return Promise.all(
    [...direct].map(async (id) => {
      const pkg = byId.get(id);
      if (pkg === undefined) throw new Error(`${id} is missing from cargo metadata.`);
      if (!pkg.license) throw new Error(`${pkg.name}@${pkg.version} declares no license; review it.`);
      const directory = dirname(pkg.manifest_path);
      const { file, excerpt } = await licenseFile(directory);
      const vcs = join(directory, ".cargo_vcs_info.json");
      const inRepo = existsSync(vcs) ? text((await readJson(vcs)).path_in_vcs) : undefined;
      const docsRs = `https://docs.rs/crate/${pkg.name}/${pkg.version}/source/${file ?? "Cargo.toml"}`;
      return {
        name: pkg.name,
        version: pkg.version,
        license: pkg.license,
        licenseUrl: await githubLicense(githubUrl(pkg.repository), inRepo, file, docsRs),
        excerpt,
      };
    }),
  );
}

/** Resolve through the workspace's node_modules symlink, then the root one. */
async function packageDirectory(name: string, workspace: string): Promise<string> {
  for (const base of [workspace, root]) {
    try {
      return dirname(await realpath(join(base, "node_modules", name, "package.json")));
    } catch {
      // Not linked from this node_modules; try the next one.
    }
  }
  throw new Error(`${name} (declared by ${workspace}) is not installed. Run bun install first.`);
}

async function describePackage(directory: string): Promise<License> {
  const pkg = await readJson(join(directory, "package.json"));
  const name = text(pkg.name);
  const version = text(pkg.version);
  const license = text(pkg.license) ?? (isObject(pkg.license) ? text(pkg.license.type) : undefined);
  if (name === undefined || version === undefined) {
    throw new Error(`${directory}/package.json has no name or version.`);
  }
  if (license === undefined) throw new Error(`${name}@${version} declares no license; review it.`);
  const { file, excerpt } = await licenseFile(directory);
  const repository = pkg.repository;
  const repo = githubUrl(
    text(repository) ?? (isObject(repository) ? text(repository.url) : undefined) ?? text(pkg.homepage),
  );
  const inRepo = isObject(repository) ? text(repository.directory)?.replace(/^\//, "") : undefined;
  const unpkg = `https://unpkg.com/${name}@${version}/${file ?? "package.json"}`;
  return { name, version, license, licenseUrl: await githubLicense(repo, inRepo, file, unpkg), excerpt };
}

/** Direct dependencies of the root package and every Bun workspace. */
async function packages(): Promise<License[]> {
  const manifest = await readJson(join(root, "package.json"));
  const workspaces = Array.isArray(manifest.workspaces) ? manifest.workspaces : [];
  const directories = new Set<string>();
  for (const workspace of [root, ...workspaces.filter((w) => typeof w === "string").map((w) => join(root, w))]) {
    const pkg = await readJson(join(workspace, "package.json"));
    for (const group of ["dependencies", "devDependencies", "peerDependencies"]) {
      const deps = pkg[group];
      for (const name of Object.keys(isObject(deps) ? deps : {})) {
        if (!name.startsWith("@clanka/") && !name.startsWith("@types/")) {
          directories.add(await packageDirectory(name, workspace));
        }
      }
    }
  }
  return Promise.all([...directories].map(describePackage));
}

const [rust, js] = await Promise.all([crates(), packages()]);
const output =
  "// Generated by bun run --cwd docs licenses; do not edit.\n" +
  `export const crates = ${JSON.stringify(sorted(rust), null, 2)} as const;\n\n` +
  `export const packages = ${JSON.stringify(sorted(js), null, 2)} as const;\n`;

if (process.argv.includes("--check")) {
  if (!existsSync(destination) || (await readFile(destination, "utf8")) !== output) {
    throw new Error("docs/src/lib/licenses.ts is stale. Run bun run --cwd docs licenses.");
  }
} else {
  await writeFile(destination, output);
}
