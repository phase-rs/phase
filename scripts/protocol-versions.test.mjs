import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import { closeSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { addProtocolChange, checkProtocolHistory, checkProtocolVersions, generateProtocolVersions, loadProtocolVersions } from "./protocol-versions.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const baseline = { full: 159, wire: 141, lobby: 16, draft: 30, directory: 1, minimumLobby: 2 };
const outputPath = "client/src/generated/protocol-versions.ts";
const uuidName = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.json$/;

function fixture(t) {
  const path = mkdtempSync(join(realpathSync(tmpdir()), "protocol-versions-"));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  mkdirSync(join(path, "protocol/changes"), { recursive: true });
  mkdirSync(join(path, "scripts"));
  copyFileSync(join(root, "protocol/baseline.json"), join(path, "protocol/baseline.json"));
  copyFileSync(join(root, "scripts/protocol-versions.mjs"), join(path, "scripts/protocol-versions.mjs"));
  copyFileSync(join(root, ".gitignore"), join(path, ".gitignore"));
  writeFileSync(join(path, "protocol/changes/.gitkeep"), "");
  return path;
}

function cli(path, ...args) {
  return execFileSync(process.execPath, [join(path, "scripts/protocol-versions.mjs"), ...args], {
    cwd: tmpdir(), encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
  });
}

function record(path, value = { surfaces: ["full", "wire"], reason: "same reason" }) {
  const file = join(path, "protocol/changes", `${crypto.randomUUID()}.json`);
  writeFileSync(file, JSON.stringify(value));
  return file;
}

function snapshot(path) {
  return Object.fromEntries(readdirSync(join(path, "protocol/changes")).sort().map((name) => [name, readFileSync(join(path, "protocol/changes", name), "utf8")]));
}

function git(path, ...args) {
  return execFileSync("git", ["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false", "-c", "user.name=Protocol Test", "-c", "user.email=protocol@example.test", ...args], {
    cwd: path, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" },
  });
}

test("empty registry preserves migration values and CLI root is independent of cwd", (t) => {
  const path = fixture(t);
  assert.deepEqual(loadProtocolVersions(path), baseline);
  assert.deepEqual(JSON.parse(cli(path, "show", "--json")), baseline);
  assert.match(cli(path, "--help"), /Equal-count unmerged branches/);
  for (const surface of ["full", "wire", "lobby", "draft", "directory"]) {
    const single = fixture(t);
    addProtocolChange(single, [surface], "one surface");
    assert.deepEqual(loadProtocolVersions(single), { ...baseline, [surface]: baseline[surface] + 1 });
  }
});

test("rejects malformed baseline values and missing inputs with path context", async (t) => {
  const invalid = [null, [], 5, { ...baseline, extra: 1 }, { ...baseline, full: undefined },
    ...[-1, 1.5, 4294967296, "159", null, true].map((full) => ({ ...baseline, full })),
    { ...baseline, minimumLobby: 17 }];
  for (const value of invalid) {
    await t.test(JSON.stringify(value), (t) => {
      const path = fixture(t);
      record(path);
      assert.equal(loadProtocolVersions(path).full, 160);
      writeFileSync(join(path, "protocol/baseline.json"), JSON.stringify(value));
      assert.throws(() => loadProtocolVersions(path), /baseline\.json/);
      assert.throws(() => cli(path, "show", "--json"), (error) => error.status !== 0 && /baseline\.json/.test(error.stderr));
    });
  }
  for (const missing of ["protocol/baseline.json", "protocol/changes"]) {
    const path = fixture(t);
    assert.deepEqual(loadProtocolVersions(path), baseline);
    rmSync(join(path, missing), { recursive: true });
    assert.throws(() => loadProtocolVersions(path), (error) => error.message.includes(join(path, missing)));
  }
  const path = fixture(t);
  writeFileSync(join(path, "protocol/baseline.json"), "{");
  assert.throws(() => loadProtocolVersions(path), /baseline\.json/);
});

test("rejects malformed records instead of silently dropping increments", async (t) => {
  const invalid = [null, [], { surfaces: ["full"] }, { surfaces: ["full"], reason: "ok", extra: true },
    ...[[], ["full", "full"], ["wire", "full"], ["game"], ["minimumLobby"], [null], "full"].map((surfaces) => ({ surfaces, reason: "ok" })),
    ...[null, 1, " \n"].map((reason) => ({ surfaces: ["full"], reason }))];
  for (const value of invalid) {
    await t.test(JSON.stringify(value), (t) => {
      const path = fixture(t);
      const file = record(path);
      assert.equal(loadProtocolVersions(path).full, 160);
      writeFileSync(file, JSON.stringify(value));
      assert.throws(() => loadProtocolVersions(path), (error) => error.message.includes(file));
    });
  }
  for (const badEntry of ["unexpected.txt", "ABC.json", "00000000-0000-1000-8000-000000000000.json", "directory", "symlink", ".gitkeep", "malformed-json"]) {
    const path = fixture(t);
    const valid = record(path);
    assert.equal(loadProtocolVersions(path).full, 160);
    const badPath = badEntry === "malformed-json" ? valid : join(path, "protocol/changes", badEntry);
    if (badEntry === "directory") mkdirSync(badPath);
    else if (badEntry === "symlink") symlinkSync(valid, badPath);
    else writeFileSync(badPath, "{");
    assert.throws(() => loadProtocolVersions(path), (error) => error.message.includes(badPath));
  }
});

test("u32 bound rejects prospective overflow before writing and leaves other axes usable", (t) => {
  const path = fixture(t);
  writeFileSync(join(path, "protocol/baseline.json"), JSON.stringify({ ...baseline, full: 4294967294 }));
  addProtocolChange(path, ["full"], "at maximum");
  assert.equal(loadProtocolVersions(path).full, 4294967295);
  const before = snapshot(path);
  assert.throws(() => addProtocolChange(path, ["game"], "overflow"), /full exceeds u32/);
  assert.deepEqual(snapshot(path), before);
  assert.deepEqual(addProtocolChange(path, ["wire"], "other axis").versions, { ...baseline, full: 4294967295, wire: 142 });
  record(path, { surfaces: ["full"], reason: "overflow on disk" });
  assert.throws(() => loadProtocolVersions(path), /\.json: full exceeds u32/);
});

test("bump authors one normalized UUID record, retains prior bytes, and regenerates", (t) => {
  const path = fixture(t);
  const baselineBefore = readFileSync(join(path, "protocol/baseline.json"), "utf8");
  cli(path, "bump", "game", "full", "wire", "lobby", "--reason", "  same reason  ");
  const first = snapshot(path);
  const name = Object.keys(first).find((name) => name !== ".gitkeep");
  assert.match(name, uuidName);
  assert.deepEqual(JSON.parse(first[name]), { surfaces: ["full", "lobby", "wire"], reason: "same reason" });
  assert.deepEqual(checkProtocolVersions(path), { ...baseline, full: 160, wire: 142, lobby: 17 });
  const second = addProtocolChange(path, ["game", "lobby"], "same reason");
  assert.equal(Object.keys(snapshot(path)).length, 3);
  assert.equal(snapshot(path)[name], first[name]);
  assert.equal(readFileSync(join(path, "protocol/baseline.json"), "utf8"), baselineBefore);
  assert.deepEqual(second.versions, { ...baseline, full: 161, wire: 143, lobby: 18 });
  assert.equal(existsSync(second.path), true);
});

test("invalid CLI/API authoring arguments leave the registry and output unchanged", (t) => {
  const path = fixture(t);
  cli(path, "bump", "game", "--reason", "control");
  const before = snapshot(path);
  const output = readFileSync(join(path, outputPath), "utf8");
  const invalid = [["bump"], ["bump", "game"], ["bump", "game", "--reason"], ["bump", "game", "--reason", " "],
    ["bump", "bogus", "--reason", "why"], ["bump", "game", "--reason", "why", "extra"],
    ["bump", "game", "--bogus", "--reason", "why"], ["bump", "game", "--reason", "--unknown"],
    ["bump", "game", "--reason", "why", "--reason", "again"], ["unknown"], ["show", "--unknown"],
    ["generate", "extra"], ["check", "extra"], ["--help", "extra"]];
  for (const args of invalid) assert.throws(() => cli(path, ...args), (error) => error.status !== 0 && error.stderr.length > 0);
  for (const [selected, reason] of [[[], "why"], ["game", "why"], [["unknown"], "why"], [["full"], null]]) {
    assert.throws(() => addProtocolChange(path, selected, reason));
  }
  assert.deepEqual(snapshot(path), before);
  assert.equal(readFileSync(join(path, outputPath), "utf8"), output);
});

test("exclusive UUID creation rejects a collision without replacing the existing record", (t) => {
  const path = fixture(t);
  const original = addProtocolChange(path, ["full"], "original");
  const before = snapshot(path);
  const uuid = original.path.split("/").at(-1).replace(".json", "");
  const originalRandomUUID = crypto.randomUUID;
  try {
    crypto.randomUUID = () => uuid;
    syncBuiltinESMExports();
    assert.throws(() => addProtocolChange(path, ["wire"], "collision"), { code: "EEXIST" });
  } finally {
    crypto.randomUUID = originalRandomUUID;
    syncBuiltinESMExports();
  }
  assert.deepEqual(snapshot(path), before);
  assert.deepEqual(loadProtocolVersions(path), { ...baseline, full: 160 });
});

test("a generation failure retains the authored record and tells the operator how to recover", (t) => {
  const path = fixture(t);
  mkdirSync(join(path, "client"));
  writeFileSync(join(path, "client/src"), "blocks generation");
  assert.throws(() => cli(path, "bump", "game", "--reason", "retained"), (error) => {
    assert.match(error.stderr, /Record retained at .*\.json/);
    assert.match(error.stderr, /generate; do not bump again/);
    return error.status !== 0;
  });
  assert.deepEqual(loadProtocolVersions(path), { ...baseline, full: 160, wire: 142 });
  rmSync(join(path, "client/src"));
  cli(path, "generate");
  cli(path, "check");
  assert.equal(Object.keys(snapshot(path)).length, 2);
});

test("generation renders exact exports, skips identical writes, and check never repairs output", (t) => {
  const path = fixture(t);
  const output = join(path, outputPath);
  assert.throws(() => checkProtocolVersions(path), /missing or stale/);
  assert.equal(existsSync(output), false);
  cli(path, "generate");
  cli(path, "check");
  const expected = "// Generated by scripts/protocol-versions.mjs; do not edit.\n"
    + "export const PROTOCOL_VERSION = 159 as const;\n"
    + "export const WIRE_PROTOCOL_VERSION = 141 as const;\n"
    + "export const LOBBY_PROTOCOL_VERSION = 16 as const;\n"
    + "export const DRAFT_PROTOCOL_VERSION = 30 as const;\n"
    + "export const DIRECTORY_VERSION = 1 as const;\n"
    + "export const MIN_SUPPORTED_LOBBY_PROTOCOL = 2 as const;\n";
  assert.equal(readFileSync(output, "utf8"), expected);
  utimesSync(output, new Date(1000000), new Date(1000000));
  const mtime = statSync(output).mtimeMs;
  generateProtocolVersions(path);
  assert.equal(statSync(output).mtimeMs, mtime);
  addProtocolChange(path, ["directory"], "directory only");
  assert.throws(() => cli(path, "check"), (error) => error.status !== 0 && /missing or stale/.test(error.stderr));
  assert.equal(readFileSync(output, "utf8"), expected);
  generateProtocolVersions(path);
  assert.equal(readFileSync(output, "utf8"), expected.replace("DIRECTORY_VERSION = 1", "DIRECTORY_VERSION = 2"));
  assert.deepEqual(checkProtocolVersions(path), { ...baseline, directory: 2 });
  git(path, "init", "-b", "main");
  assert.equal(git(path, "check-ignore", outputPath).trim(), outputPath);
  git(path, "add", ".");
  assert.equal(git(path, "ls-files", outputPath), "");
});

test("generation replaces output while existing readers retain the complete prior module", (t) => {
  const path = fixture(t);
  const output = join(path, outputPath);
  generateProtocolVersions(path);
  const previous = readFileSync(output, "utf8");
  const reader = openSync(output, "r");
  t.after(() => closeSync(reader));
  addProtocolChange(path, ["directory"], "new directory version");
  generateProtocolVersions(path);
  assert.equal(readFileSync(reader, "utf8"), previous);
  assert.equal(readFileSync(output, "utf8"), previous.replace("DIRECTORY_VERSION = 1", "DIRECTORY_VERSION = 2"));
  assert.deepEqual(readdirSync(dirname(output)), ["protocol-versions.ts"]);
  checkProtocolVersions(path);
});

test("import has no CLI or filesystem side effects even with CLI-like argv", (t) => {
  const path = fixture(t);
  rmSync(join(path, "protocol"), { recursive: true });
  const scriptUrl = pathToFileURL(join(path, "scripts/protocol-versions.mjs")).href;
  const result = spawnSync(process.execPath, ["--input-type=module", "-e",
    `process.argv = ['node', '/unrelated/entry.mjs', 'bump', 'game', '--reason', 'no']; await import(${JSON.stringify(scriptUrl)});`],
  { cwd: path, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "");
  assert.equal(result.stderr, "");
  assert.equal(existsSync(join(path, "protocol")), false);
  assert.equal(existsSync(join(path, outputPath)), false);
});

for (const [label, left, right, expected] of [
  ["same game surfaces", ["game"], ["game"], { ...baseline, full: 161, wire: 143 }],
  ["mixed surfaces", ["game", "lobby"], ["wire", "draft"], { ...baseline, full: 160, wire: 143, lobby: 17, draft: 31 }],
]) {
  test(`independent Git branches merge additive UUID identities: ${label}`, (t) => {
    const path = fixture(t);
    git(path, "init", "-b", "main");
    git(path, "add", ".");
    git(path, "commit", "-m", "baseline");
    git(path, "checkout", "-b", "left");
    cli(path, "bump", ...left, "--reason", "identical reason");
    const leftRecords = snapshot(path);
    const leftValues = JSON.parse(cli(path, "show", "--json"));
    assert.deepEqual(leftValues, { ...baseline, full: 160, wire: 142, ...(left.includes("lobby") ? { lobby: 17 } : {}) });
    git(path, "add", "protocol/changes");
    git(path, "commit", "-m", "left record");
    git(path, "checkout", "-b", "right", "main");
    cli(path, "bump", ...right, "--reason", "identical reason");
    const rightRecords = snapshot(path);
    const rightValues = JSON.parse(cli(path, "show", "--json"));
    assert.deepEqual(rightValues, right.includes("game") ? { ...baseline, full: 160, wire: 142 } : { ...baseline, wire: 142, draft: 31 });
    if (label === "same game surfaces") assert.deepEqual(leftValues, rightValues);
    git(path, "add", "protocol/changes");
    git(path, "commit", "-m", "right record");
    git(path, "merge", "--no-edit", "left");
    assert.deepEqual(snapshot(path), { ...leftRecords, ...rightRecords });
    assert.equal(Object.keys(snapshot(path)).length, 3);
    assert.deepEqual(JSON.parse(cli(path, "show", "--json")), expected);
    cli(path, "generate");
    assert.deepEqual(checkProtocolVersions(path), expected);
    assert.equal(git(path, "status", "--porcelain"), "");
    checkProtocolHistory(path, "left");
    checkProtocolHistory(path, "right");
    const prior = Object.keys(leftRecords).find((name) => name !== ".gitkeep");
    rmSync(join(path, "protocol/changes", prior));
    cli(path, "generate");
    assert.throws(() => checkProtocolHistory(path, "left"), (error) => error.message.includes(prior));
  });
}

function ownershipFixture(path) {
  for (const file of ["scripts/check-protocol-version.mjs", "crates/lobby-broker/src/protocol.rs",
    "crates/lobby-broker/src/lib.rs", "crates/lobby-broker/src/directory.rs", "crates/server-core/src/protocol.rs",
    "client/src/adapter/ws-adapter.ts", "client/src/network/protocol.ts", "client/src/network/draftProtocol.ts",
    "crates/draft-core/src/types.rs", "client/src/services/serverDirectory.ts", "scripts/card-bot/servers.ts",
    "lobby-worker/src/hello-gate.ts"]) {
    mkdirSync(dirname(join(path, file)), { recursive: true });
    copyFileSync(join(root, file), join(path, file));
  }
  generateProtocolVersions(path);
  checker(path);
}

function checker(path, ...args) {
  return execFileSync(process.execPath, [join(path, "scripts/check-protocol-version.mjs"), ...args], {
    cwd: tmpdir(), encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
  });
}

function commit(path) {
  git(path, "add", ".");
  git(path, "commit", "-m", "fixture");
  return git(path, "rev-parse", "HEAD").trim();
}

test("history preserves every path and byte while allowing additions and explicit later bases", (t) => {
  const path = fixture(t);
  ownershipFixture(path);
  git(path, "init", "-b", "main");
  const first = record(path);
  const second = record(path, { surfaces: ["draft"], reason: "second" });
  const base = commit(path);
  git(path, "tag", "snapshot", base);
  const added = record(path);
  generateProtocolVersions(path);
  checker(path, "--base", "snapshot");
  const later = commit(path);
  rmSync(added);
  generateProtocolVersions(path);
  checker(path, "--base", base);
  assert.throws(() => checker(path, "--base", later), (e) => e.stderr.includes(added.split("/").at(-1)) && e.stderr.includes(later));
  const baselinePath = join(path, "protocol/baseline.json");
  for (const [file, mutate] of [
    [first, (file) => rmSync(file)],
    [second, (file) => { const value = readFileSync(file); rmSync(file); writeFileSync(record(path), value); }],
    [first, (file) => writeFileSync(file, JSON.stringify({ surfaces: ["full"], reason: "same reason" }))],
    [second, (file) => writeFileSync(file, JSON.stringify({ surfaces: ["draft"], reason: "changed" }))],
    [first, (file) => writeFileSync(file, `${readFileSync(file)}\n`)],
    [baselinePath, (file) => writeFileSync(file, `${readFileSync(file)}\n`)],
    [baselinePath, (file) => writeFileSync(file, JSON.stringify({ ...baseline, full: 160 }))],
  ]) {
    const bytes = readFileSync(file);
    checker(path, "--base", base);
    mutate(file);
    generateProtocolVersions(path);
    checker(path);
    assert.throws(() => checker(path, "--base", base), (e) => e.stderr.includes(file.split("/").at(-1)) && e.stderr.includes(base));
    writeFileSync(file, bytes);
    generateProtocolVersions(path);
  }
  const bytes = readFileSync(baselinePath);
  const target = join(path, "baseline-copy");
  writeFileSync(target, bytes);
  rmSync(baselinePath);
  symlinkSync(target, baselinePath);
  assert.throws(() => checkProtocolHistory(path, base), /baseline.json: prior file must remain a regular file/);
});

test("checker rejects unsupported argv and unresolved, option-like or noncommit bases", (t) => {
  const path = fixture(t);
  ownershipFixture(path);
  git(path, "init", "-b", "main");
  const base = commit(path);
  checker(path, "--base", base);
  for (const args of [["--base"], ["--base", ""], ["--base", " "], ["--other"],
    ["--base", base, "extra"], ["--base", base, "--base", base]]) {
    assert.throws(() => checker(path, ...args), (e) => /Usage:/.test(e.stderr));
  }
  for (const ref of ["missing", "--help", `${base}:protocol/baseline.json`]) {
    assert.throws(() => checker(path, "--base", ref), (e) => e.stderr.includes(`cannot resolve base ${ref}`));
  }
  assert.throws(() => checkProtocolHistory(path, ""), /nonempty base/);
});

function migrationSource(path, values) {
  const files = {
    "crates/lobby-broker/src/protocol.rs": `pub const PROTOCOL_VERSION: u32 = ${values.full};\npub const LOBBY_PROTOCOL_VERSION: u32 = ${values.lobby};\npub const MIN_SUPPORTED_LOBBY_PROTOCOL: u32 = ${values.minimumLobby};\n`,
    "client/src/network/protocol.ts": `export const WIRE_PROTOCOL_VERSION = ${values.wire};\n`,
    "client/src/network/draftProtocol.ts": `export const DRAFT_PROTOCOL_VERSION = ${values.draft} as const;\n`,
    "crates/lobby-broker/src/directory.rs": `pub const DIRECTORY_VERSION: u32 = ${values.directory};\n`,
  };
  for (const [file, source] of Object.entries(files)) {
    mkdirSync(dirname(join(path, file)), { recursive: true });
    writeFileSync(join(path, file), source);
  }
}

test("migration reads all six historical literals, including varied values and appended changes", (t) => {
  for (const values of [baseline, { full: 201, wire: 182, lobby: 22, draft: 45, directory: 8, minimumLobby: 3 }]) {
    const path = fixture(t);
    rmSync(join(path, "protocol"), { recursive: true });
    migrationSource(path, values);
    git(path, "init", "-b", "main");
    const base = commit(path);
    mkdirSync(join(path, "protocol/changes"), { recursive: true });
    const file = join(path, "protocol/baseline.json");
    writeFileSync(file, JSON.stringify(values));
    record(path);
    ownershipFixture(path);
    checker(path, "--base", base);
    assert.equal(loadProtocolVersions(path).full, values.full + 1);
    for (const key of Object.keys(values)) {
      writeFileSync(file, JSON.stringify({ ...values, [key]: values[key] + 1 }));
      generateProtocolVersions(path);
      assert.throws(() => checker(path, "--base", base), (e) => e.stderr.includes(`${key} differs from historical`));
    }
  }
});

test("migration rejects missing, renamed, expression, duplicate and partial historical inputs", (t) => {
  for (const kind of ["missing", "renamed", "expression", "duplicate", "partial"]) {
    const path = fixture(t);
    rmSync(join(path, "protocol"), { recursive: true });
    migrationSource(path, baseline);
    git(path, "init", "-b", "main");
    const valid = commit(path);
    const file = join(path, "client/src/network/protocol.ts");
    if (kind === "missing") rmSync(file);
    if (kind === "renamed") writeFileSync(file, "export const RENAMED = 141;\n");
    if (kind === "expression") writeFileSync(file, "export const WIRE_PROTOCOL_VERSION = 140 + 1;\n");
    if (kind === "duplicate") writeFileSync(file, readFileSync(file, "utf8").repeat(2));
    if (kind === "partial") { mkdirSync(join(path, "protocol/changes"), { recursive: true }); record(path); }
    const bad = commit(path);
    mkdirSync(join(path, "protocol/changes"), { recursive: true });
    writeFileSync(join(path, "protocol/baseline.json"), JSON.stringify(baseline));
    ownershipFixture(path);
    checker(path, "--base", valid);
    assert.throws(() => checker(path, "--base", bad), (e) => e.stderr.includes(bad) && /historical|bare integer/.test(e.stderr));
  }
});

function integrationShell() {
  const workflow = readFileSync(join(root, ".github/workflows/ci.yml"), "utf8");
  const frontend = workflow.split("  frontend:\n")[1].split("\n  draft-match-wasm-proof:")[0];
  assert.match(frontend, /uses: actions\/checkout@v4\n        with:\n          fetch-depth: 0/);
  assert.match(frontend, /run: node --test scripts\/protocol-versions.test.mjs scripts\/engine-fingerprint.test.mjs/);
  assert.match(workflow.split("  pull_request:")[0], /- 'protocol\/\*\*'/);
  const blocks = [...frontend.matchAll(/      - name: Protocol integration history\n([\s\S]*?)(?=\n      - name:)/g)];
  assert.equal(blocks.length, 1);
  const block = blocks[0][1];
  for (const [key, field] of [["EVENT", "event_name"], ["PR_BASE", "event.pull_request.base.sha"],
    ["MERGE_BASE", "event.merge_group.base_sha"], ["PUSH_BASE", "event.before"]]) {
    assert.ok(block.includes("PROTOCOL_" + key + ": ${{ github." + field + " }}"));
  }
  assert.doesNotMatch(block, /continue-on-error:|\n\s+if:/);
  const shell = block.split("        run: |\n")[1].split("\n").map((line) => line.replace(/^          /, "")).join("\n");
  assert.ok(shell.trim());
  return shell;
}

function runIntegration(path, env) {
  return spawnSync("bash", ["-c", integrationShell()], {
    cwd: path, encoding: "utf8", env: { ...process.env, PROTOCOL_PR_BASE: "", PROTOCOL_MERGE_BASE: "", PROTOCOL_PUSH_BASE: "", ...env },
  });
}

test("actual CI shell selects each event snapshot and enforces even nonancestor push history", (t) => {
  const path = fixture(t);
  ownershipFixture(path);
  git(path, "init", "-b", "main");
  const initial = commit(path);
  const branches = [];
  for (const event of ["pull_request", "merge_group", "push"]) {
    git(path, "checkout", "-b", event, initial);
    const file = record(path);
    branches.push({ event, file, bytes: readFileSync(file), sha: commit(path) });
  }
  git(path, "checkout", "main");
  for (const { file, bytes } of branches) writeFileSync(file, bytes);
  git(path, "update-ref", "refs/remotes/origin/main", initial);
  const fields = { PROTOCOL_PR_BASE: branches[0].sha, PROTOCOL_MERGE_BASE: branches[1].sha, PROTOCOL_PUSH_BASE: branches[2].sha };
  for (const { event, file, bytes, sha } of branches) {
    const env = { ...fields, PROTOCOL_EVENT: event };
    let result = runIntegration(path, env);
    assert.equal(result.status, 0, result.stderr);
    rmSync(file);
    result = runIntegration(path, env);
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes(file.split("/").at(-1)) && result.stderr.includes(sha), result.stderr);
    writeFileSync(file, bytes);
  }
  for (const [event, field] of [["push", "PROTOCOL_PUSH_BASE"], ["pull_request", "PROTOCOL_PR_BASE"], ["merge_group", "PROTOCOL_MERGE_BASE"]]) {
    for (const invalid of ["", "invalid", "f".repeat(40), ...(event === "push" ? [] : ["0".repeat(40)])]) {
      const result = runIntegration(path, { ...fields, PROTOCOL_EVENT: event, [field]: invalid });
      assert.notEqual(result.status, 0, `${event} ${invalid}`);
      assert.match(result.stderr, /Invalid protocol base|origin|remote/);
    }
  }
  assert.notEqual(runIntegration(path, { ...fields, PROTOCOL_EVENT: "unknown" }).status, 0);
  const firstPush = { ...fields, PROTOCOL_EVENT: "push", PROTOCOL_PUSH_BASE: "0".repeat(40) };
  const bootstrap = runIntegration(path, firstPush);
  assert.equal(bootstrap.status, 0, bootstrap.stderr);
  assert.match(bootstrap.stdout, /Branch creation has no predecessor/);
  writeFileSync(join(path, "client/src/network/protocol.ts"), "export const WIRE_PROTOCOL_VERSION = 141;\n");
  assert.notEqual(runIntegration(path, firstPush).status, 0);
});

test("actual CI shell fetches the exact absent base from origin", (t) => {
  const origin = fixture(t);
  ownershipFixture(origin);
  git(origin, "init", "-b", "main");
  commit(origin);
  const file = record(origin);
  const base = commit(origin);
  const path = fixture(t);
  ownershipFixture(path);
  git(path, "init", "-b", "main");
  commit(path);
  git(path, "remote", "add", "origin", origin);
  assert.throws(() => git(path, "cat-file", "-e", `${base}^{commit}`));
  const current = join(path, "protocol/changes", file.split("/").at(-1));
  copyFileSync(file, current);
  const env = { PROTOCOL_EVENT: "push", PROTOCOL_PUSH_BASE: base };
  const result = runIntegration(path, env);
  assert.equal(result.status, 0, result.stderr);
  git(path, "cat-file", "-e", `${base}^{commit}`);
  rmSync(current);
  assert.notEqual(runIntegration(path, env).status, 0);
});

test("history refuses malformed historical entries and current record symlinks", (t) => {
  for (const kind of ["unexpected", "marker", "symlink", "malformed"]) {
    const path = fixture(t);
    ownershipFixture(path);
    git(path, "init", "-b", "main");
    const first = record(path);
    const bytes = readFileSync(first);
    const valid = commit(path);
    let bad = first;
    if (kind === "unexpected") bad = join(path, "protocol/unexpected");
    if (kind === "marker") bad = join(path, "protocol/changes/.gitkeep");
    if (kind === "symlink") {
      rmSync(first);
      symlinkSync("../baseline.json", first);
    } else writeFileSync(bad, "invalid");
    const invalid = commit(path);
    rmSync(bad);
    writeFileSync(first, bytes);
    if (kind === "marker") writeFileSync(bad, "");
    generateProtocolVersions(path);
    checker(path, "--base", valid);
    assert.throws(() => checker(path, "--base", invalid), (e) => e.stderr.includes(invalid) && e.stderr.includes(bad.split("/").at(-1)));
    rmSync(first);
    symlinkSync("../baseline.json", first);
    assert.throws(() => checker(path, "--base", valid), (e) => e.stderr.includes(valid) && e.stderr.includes(first.split("/").at(-1)));
  }
});
