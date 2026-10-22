#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const surfaces = ["full", "wire", "lobby", "draft", "directory"];
const baselineKeys = [...surfaces, "minimumLobby"];
const maxVersion = 0xffffffff;
const recordName = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.json$/;
const generatedPath = "client/src/generated/protocol-versions.ts";

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`${path}: ${error.message}`);
  }
}

function requireKeys(value, keys, path) {
  if (value === null || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== keys.length
    || keys.some((key) => !Object.hasOwn(value, key))) {
    throw new Error(`${path}: expected exactly ${keys.join(", ")}`);
  }
}

function requireReason(reason, path) {
  if (typeof reason !== "string" || reason.trim().length === 0) {
    throw new Error(`${path}: reason must be a nonempty string`);
  }
}

function increment(versions, selected, path) {
  for (const surface of selected) {
    if (versions[surface] === maxVersion) {
      throw new Error(`${path}: ${surface} exceeds u32 maximum`);
    }
    versions[surface] += 1;
  }
}

export function loadProtocolVersions(root = repositoryRoot) {
  const baselinePath = join(root, "protocol/baseline.json");
  const baseline = readJson(baselinePath);
  requireKeys(baseline, baselineKeys, baselinePath);
  for (const key of baselineKeys) {
    if (!Number.isInteger(baseline[key]) || baseline[key] < 0 || baseline[key] > maxVersion) {
      throw new Error(`${baselinePath}: ${key} must be a u32 integer`);
    }
  }
  if (baseline.minimumLobby > baseline.lobby) {
    throw new Error(`${baselinePath}: minimumLobby must not exceed lobby`);
  }
  const versions = Object.fromEntries(baselineKeys.map((key) => [key, baseline[key]]));
  const changesPath = join(root, "protocol/changes");
  const entries = readdirSync(changesPath, { withFileTypes: true }).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
  for (const entry of entries) {
    const path = join(changesPath, entry.name);
    if (!entry.isFile()) throw new Error(`${path}: expected a regular record file`);
    if (entry.name === ".gitkeep") {
      if (statSync(path).size !== 0) throw new Error(`${path}: marker must be empty`);
      continue;
    }
    if (!recordName.test(entry.name)) throw new Error(`${path}: expected a lowercase UUID v4 .json filename`);
    const record = readJson(path);
    requireKeys(record, ["surfaces", "reason"], path);
    if (!Array.isArray(record.surfaces) || record.surfaces.length === 0
      || record.surfaces.some((surface, index) => !surfaces.includes(surface)
        || (index > 0 && record.surfaces[index - 1] >= surface))) {
      throw new Error(`${path}: surfaces must be nonempty, unique, sorted protocol surface identifiers`);
    }
    requireReason(record.reason, path);
    increment(versions, record.surfaces, path);
  }
  return versions;
}

// History is an integration concern; ordinary generation never invokes Git.
export function checkProtocolHistory(root, baseRef) {
  if (typeof baseRef !== "string" || baseRef.trim().length === 0) {
    throw new Error("Protocol history requires a nonempty base ref");
  }
  const git = (...args) => execFileSync("git", args, {
    cwd: root, stdio: ["ignore", "pipe", "pipe"],
  });
  let base;
  try {
    base = git("rev-parse", "--verify", "--end-of-options", `${baseRef}^{commit}`).toString().trim();
  } catch {
    throw new Error(`Protocol history: cannot resolve base ${baseRef} to a commit`);
  }
  const fail = (path, message) => {
    throw new Error(`Protocol history base ${baseRef} (${base}): ${path}: ${message}`);
  };
  try { loadProtocolVersions(root); }
  catch (error) { fail("current registry", error.message); }
  const entries = git("ls-tree", "-r", "-z", base, "--", "protocol").toString().split("\0").filter(Boolean).map((entry) => {
    const [header, path] = entry.split("\t");
    const [mode, type] = header.split(" ");
    return { mode, type, path };
  });
  const blob = (path) => {
    try { return git("show", `${base}:${path}`); }
    catch { fail(path, "cannot read historical file"); }
  };
  if (!entries.some(({ path }) => path === "protocol/baseline.json")) {
    if (entries.length) fail(entries[0].path, "partial historical registry has no baseline");
    const baseline = readJson(join(root, "protocol/baseline.json"));
    const declarations = [
      ["full", "crates/lobby-broker/src/protocol.rs", "PROTOCOL_VERSION", "rust"],
      ["lobby", "crates/lobby-broker/src/protocol.rs", "LOBBY_PROTOCOL_VERSION", "rust"],
      ["minimumLobby", "crates/lobby-broker/src/protocol.rs", "MIN_SUPPORTED_LOBBY_PROTOCOL", "rust"],
      ["wire", "client/src/network/protocol.ts", "WIRE_PROTOCOL_VERSION", "ts"],
      ["draft", "client/src/network/draftProtocol.ts", "DRAFT_PROTOCOL_VERSION", "ts"],
      ["directory", "crates/lobby-broker/src/directory.rs", "DIRECTORY_VERSION", "rust"],
    ];
    for (const [key, path, name, language] of declarations) {
      const prefix = language === "rust" ? `pub\\s+const\\s+${name}\\s*:\\s*u32` : `export\\s+const\\s+${name}`;
      const matches = [...blob(path).toString().matchAll(new RegExp(`^${prefix}\\s*=\\s*([^;]+);`, "gm"))];
      const literal = language === "rust" ? /^(\d+)$/ : /^(\d+)(?:\s+as\s+const)?$/;
      const value = matches.length === 1 && matches[0][1].trim().match(literal);
      if (!value) fail(path, `${name} must have exactly one bare integer declaration`);
      if (Number(value[1]) !== baseline[key]) fail("protocol/baseline.json", `${key} differs from historical ${path} ${name}`);
    }
    return;
  }
  for (const { mode, type, path } of entries) {
    if (type !== "blob" || !["100644", "100755"].includes(mode)) fail(path, "expected a regular historical file");
    if (path === "protocol/changes/.gitkeep") {
      if (blob(path).length !== 0) fail(path, "marker must be empty");
      continue;
    }
    if (path !== "protocol/baseline.json"
      && !(path.startsWith("protocol/changes/") && recordName.test(path.slice("protocol/changes/".length)))) {
      fail(path, "unexpected historical registry entry");
    }
    const current = join(root, path);
    if (!existsSync(current) || !lstatSync(current).isFile()) fail(path, "prior file must remain a regular file");
    if (!readFileSync(current).equals(blob(path))) fail(path, "prior file bytes must remain unchanged");
  }
}

export function addProtocolChange(root, selected, reason) {
  if (!Array.isArray(selected) || selected.length === 0) {
    throw new Error("bump: select at least one surface");
  }
  for (const surface of selected) {
    if (surface !== "game" && !surfaces.includes(surface)) {
      throw new Error(`bump: unknown surface ${surface}`);
    }
  }
  requireReason(reason, "bump");
  const normalized = [...new Set(selected.flatMap((surface) => surface === "game" ? ["full", "wire"] : [surface]))].sort();
  const versions = loadProtocolVersions(root);
  increment(versions, normalized, "bump");
  // UUID identity lets independent branch records merge additively, even for identical reasons.
  // Retain prior records; a reversion is another change record, never a deletion.
  const path = resolve(root, "protocol/changes", `${randomUUID()}.json`);
  writeFileSync(path, `${JSON.stringify({ surfaces: normalized, reason: reason.trim() }, null, 2)}\n`, { flag: "wx" });
  return { path, versions };
}

function renderVersions(versions) {
  return `// Generated by scripts/protocol-versions.mjs; do not edit.\n`
    + `export const PROTOCOL_VERSION = ${versions.full} as const;\n`
    + `export const WIRE_PROTOCOL_VERSION = ${versions.wire} as const;\n`
    + `export const LOBBY_PROTOCOL_VERSION = ${versions.lobby} as const;\n`
    + `export const DRAFT_PROTOCOL_VERSION = ${versions.draft} as const;\n`
    + `export const DIRECTORY_VERSION = ${versions.directory} as const;\n`
    + `export const MIN_SUPPORTED_LOBBY_PROTOCOL = ${versions.minimumLobby} as const;\n`;
}

export function generateProtocolVersions(root = repositoryRoot) {
  const versions = loadProtocolVersions(root);
  const output = join(root, generatedPath);
  const contents = renderVersions(versions);
  if (!existsSync(output) || readFileSync(output, "utf8") !== contents) {
    mkdirSync(dirname(output), { recursive: true });
    const temporary = `${output}.${process.pid}.tmp`;
    writeFileSync(temporary, contents);
    renameSync(temporary, output);
  }
  return versions;
}

export function checkProtocolVersions(root = repositoryRoot) {
  const versions = loadProtocolVersions(root);
  const output = join(root, generatedPath);
  if (!existsSync(output) || readFileSync(output, "utf8") !== renderVersions(versions)) {
    throw new Error(`${output}: missing or stale; run node scripts/protocol-versions.mjs generate`);
  }
  return versions;
}

const help = `Usage: node scripts/protocol-versions.mjs <command>
  bump <surface...> --reason <text>  Record one incompatible change and regenerate
  generate                         Regenerate disposable TypeScript exports
  check                            Check generated output without writing
  show [--json]                    Report computed versions
  --help                           Show this help
Surfaces: full, wire, lobby, draft, directory; game expands to full + wire.
Independent UUID records merge additively. Commit the new record, not generated output.
Revert a protocol change by adding a new record; never delete prior records.
Equal-count unmerged branches remain numerically indistinguishable.
Integration checks: node scripts/check-protocol-version.mjs --base <ref>.
`;

function main(args) {
  const [command, ...rest] = args;
  if (command === "bump") {
    const reasonIndex = rest.indexOf("--reason");
    if (reasonIndex < 1 || reasonIndex !== rest.length - 2 || rest[reasonIndex + 1].startsWith("--")) {
      throw new Error("bump: expected <surface...> --reason <text>");
    }
    const result = addProtocolChange(repositoryRoot, rest.slice(0, reasonIndex), rest[reasonIndex + 1]);
    try {
      generateProtocolVersions();
    } catch (error) {
      throw new Error(`Record retained at ${result.path}. Run node scripts/protocol-versions.mjs generate; do not bump again. ${error.message}`);
    }
    console.log(`Created ${result.path}\n${JSON.stringify(result.versions)}`);
    return;
  }
  if (command === "show" && (rest.length === 0 || (rest.length === 1 && rest[0] === "--json"))) {
    console.log(JSON.stringify(loadProtocolVersions()));
    return;
  }
  if (rest.length !== 0) throw new Error(`Unexpected arguments for ${command}: ${rest.join(" ")}`);
  switch (command) {
    case "generate": generateProtocolVersions(); break;
    case "check": checkProtocolVersions(); break;
    case "--help": process.stdout.write(help); break;
    default: throw new Error(`Unknown command: ${command ?? "(missing)"}; use --help`);
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
