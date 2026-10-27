import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = resolve(dirname(fileURLToPath(import.meta.url)), "engine-fingerprint.sh");

test("publication fingerprint tracks registry, crates and data at the selected commit", () => {
  const root = mkdtempSync(join(tmpdir(), "phase-fingerprint-"));
  const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
  const commit = () => {
    git("add", ".");
    git("commit", "-qm", "fixture");
    return git("rev-parse", "HEAD");
  };
  const fingerprint = (ref) => execFileSync("bash", [script, ref, join(root, "cards.json"), join(root, "pools.json")], { cwd: root, encoding: "utf8" }).trim();
  try {
    git("init", "-q");
    git("config", "user.email", "fixture@example.invalid");
    git("config", "user.name", "Fixture");
    git("config", "commit.gpgsign", "false");
    mkdirSync(join(root, "protocol/changes"), { recursive: true });
    mkdirSync(join(root, "crates"));
    writeFileSync(join(root, "protocol/baseline.json"), '{"full":1}\n');
    writeFileSync(join(root, "crates/lib.rs"), "pub const VALUE: u32 = 1;\n");
    writeFileSync(join(root, "cards.json"), "[]\n");
    writeFileSync(join(root, "pools.json"), "[]\n");
    const initial = commit();
    const initialKey = fingerprint(initial);
    assert.match(initialKey, /^[0-9a-f]{16}$/);
    writeFileSync(join(root, "unrelated.txt"), "unrelated\n");
    assert.equal(fingerprint(commit()), initialKey, "commit identity does not affect content key");
    writeFileSync(join(root, "protocol/changes/00000000-0000-4000-8000-000000000001.json"), '{"surfaces":["full"],"reason":"shape"}\n');
    const registry = commit();
    const registryKey = fingerprint(registry);
    assert.notEqual(registryKey, initialKey, "registry-only commit must invalidate native publication");
    assert.equal(fingerprint(initial), initialKey, "selected ref, not working tree, owns registry input");
    writeFileSync(join(root, "crates/lib.rs"), "pub const VALUE: u32 = 2;\n");
    const crateKey = fingerprint(commit());
    assert.notEqual(crateKey, registryKey, "existing crate input remains tracked");
    writeFileSync(join(root, "Cargo.lock"), '[[package]]\nname = "webpki-roots"\nversion = "1.0.9"\n');
    const oldTrustRef = commit();
    const oldTrustKey = fingerprint(oldTrustRef);
    writeFileSync(join(root, "Cargo.lock"), '[[package]]\nname = "webpki-roots"\nversion = "1.0.10"\n');
    const trustKey = fingerprint(commit());
    assert.notEqual(trustKey, oldTrustKey, "trust-store-only update must invalidate native publication");
    assert.equal(fingerprint(oldTrustRef), oldTrustKey, "selected ref owns the locked trust store");
    writeFileSync(join(root, "cards.json"), "[1]\n");
    const cardsKey = fingerprint("HEAD");
    assert.notEqual(cardsKey, trustKey, "card data remains tracked");
    writeFileSync(join(root, "pools.json"), "[1]\n");
    assert.notEqual(fingerprint("HEAD"), cardsKey, "draft pools remain tracked");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
