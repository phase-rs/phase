import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, readdir, realpath, rm, symlink, truncate, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { brotliCompressSync } from "node:zlib";

import {
  PAGES_FREE_MAX_FILES,
  PAGES_MAX_FILE_BYTES,
  preparePagesArtifacts,
  validatePagesLimits,
} from "./prepare-cloudflare-pages.mjs";

const DATA_FILES = JSON.parse(
  await readFile(new URL("../data-files.json", import.meta.url), "utf8"),
);
const REPOSITORY_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DATA_BASE_URL = "https://assets.example.test/releases/fall-2026";
const CARD_BYTES = Buffer.from('{"cards":["fixture"]}\n');
const CARD_HASH = createHash("sha256").update(CARD_BYTES).digest("hex").slice(0, 16);
const CARD_FILENAME = `card-data-${CARD_HASH}.json`;
const ENGINE_BYTES = Buffer.from("genuine-local-wasm-fixture");
const ENGINE_HASH = createHash("sha256").update(ENGINE_BYTES).digest("hex").slice(0, 16);

const CONFIG = {
  dataBaseUrl: DATA_BASE_URL,
  cardDataUrl: `${DATA_BASE_URL}/${CARD_FILENAME}`,
  engineWasmUrl: `https://assets.example.test/wasm/engine_wasm_bg-${ENGINE_HASH}.wasm`,
};

const roots = new Set();

async function createFixture({
  bundleOverrides = {},
  includePlainCardData = true,
  includeHashedCardCorpus = true,
  includeConfiguredEngine = false,
  localWasmReference = false,
  includeManifestFiles = true,
  baseDirectory,
} = {}) {
  const root = baseDirectory ?? (await mkdtemp(path.join(await realpath(os.tmpdir()), "pages-prep-test-")));
  roots.add(root);
  const inputDir = path.join(root, "input");
  const outputDir = path.join(root, "output");
  await mkdir(path.join(inputDir, "assets"), { recursive: true });
  await writeFile(
    path.join(inputDir, "index.html"),
    '<!doctype html><html><body><div id="root"></div><script type="module" src="/assets/app.js"></script></body></html>\n',
  );

  for (const filename of DATA_FILES) {
    if (!includeManifestFiles) continue;
    await writeFile(path.join(inputDir, filename), JSON.stringify({ fixture: filename }));
  }

  if (includeHashedCardCorpus) await writeFile(path.join(inputDir, CARD_FILENAME), CARD_BYTES);
  if (includePlainCardData) await writeFile(path.join(inputDir, "card-data.json"), CARD_BYTES);

  if (includeConfiguredEngine) {
    await writeFile(
      path.join(inputDir, `engine_wasm_bg-${ENGINE_HASH}.wasm`),
      ENGINE_BYTES,
    );
  }
  if (localWasmReference) {
    await writeFile(path.join(inputDir, "assets", "draft_wasm_bg.wasm"), Buffer.from([0, 97, 115, 109]));
  }

  const defaultBundle = [
    ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
    CONFIG.cardDataUrl,
    CONFIG.engineWasmUrl,
    ...(localWasmReference ? ["./draft_wasm_bg.wasm"] : []),
  ];
  const bundle = bundleOverrides.bundle ?? defaultBundle;
  await writeFile(
    path.join(inputDir, "assets", "app.js"),
    `const runtimeUrls = ${JSON.stringify(bundle)};\n`,
  );

  return {
    root,
    inputDir,
    outputDir,
    options: {
      inputDir,
      outputDir,
      ...CONFIG,
      attestBuildUrls: true,
      ...bundleOverrides.options,
    },
  };
}

async function snapshotTree(root) {
  const rows = [];
  async function visit(current, relative) {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
    for (const entry of entries) {
      const next = path.join(current, entry.name);
      const name = relative ? `${relative}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await visit(next, name);
      else {
        const bytes = await readFile(next);
        rows.push({
          path: name,
          bytes: bytes.length,
          sha256: createHash("sha256").update(bytes).digest("hex"),
        });
      }
    }
  }
  await visit(root, "");
  return rows;
}

async function cleanup() {
  await Promise.all([...roots].map((root) => rm(root, { recursive: true, force: true })));
  roots.clear();
}

async function assertCredentialRejectedWithoutEcho(promise, secret) {
  await assert.rejects(promise, (error) => {
    assert.match(error.message, /appears to contain a credential/);
    assert.equal(error.message.includes(secret), false);
    return true;
  });
}

test.afterEach(cleanup);

test("removes exact manifest and card corpus names, preserving similar and nested files", async () => {
  const fixture = await createFixture();
  await writeFile(path.join(fixture.inputDir, "card-data-backup.json"), "keep");
  await writeFile(path.join(fixture.inputDir, "card-data-1234.json"), "keep");
  await writeFile(path.join(fixture.inputDir, "card-data-0123456789abcdef0.json"), "keep");
  await writeFile(path.join(fixture.inputDir, "card-data-0123456789abcdeg.json"), "keep");
  await writeFile(path.join(fixture.inputDir, "not-card-names.json"), "keep");
  await mkdir(path.join(fixture.inputDir, "nested"));
  await writeFile(path.join(fixture.inputDir, "nested", "card-names.json"), "keep nested");

  const report = await preparePagesArtifacts(fixture.options);
  const output = await snapshotTree(fixture.outputDir);
  const outputPaths = new Set(output.map((entry) => entry.path));

  for (const name of DATA_FILES) {
    assert.equal(outputPaths.has(name), false, `${name} should be offloaded`);
    assert.equal(outputPaths.has(`${name}.br`), false, `${name}.br should be offloaded`);
  }
  for (const name of ["card-data.json", `${CARD_FILENAME}`]) {
    assert.equal(outputPaths.has(name), false, `${name} should be offloaded`);
    assert.equal(outputPaths.has(`${name}.br`), false);
  }
  for (const name of [
    "card-data-backup.json",
    "card-data-1234.json",
    "card-data-0123456789abcdef0.json",
    "card-data-0123456789abcdeg.json",
    "not-card-names.json",
    "nested/card-names.json",
  ]) {
    assert.equal(outputPaths.has(name), true, `${name} must be preserved`);
  }

  assert.deepEqual(report.removedFromPages, [
    ...DATA_FILES,
    "card-data.json",
    `${CARD_FILENAME}`,
  ].sort());
  assert.equal(report.publicObjects.find((object) => object.key === "card-names.json").publicUrl,
    `${DATA_BASE_URL}/card-names.json`);
});

test("rejects a 16-hex card-data name whose bytes are not content-addressed by that name", async () => {
  const fixture = await createFixture();
  const unrelated = path.join(fixture.inputDir, "card-data-0000000000000000.json");
  await writeFile(unrelated, '{"not":"the named content"}');
  const before = await snapshotTree(fixture.inputDir);
  await assert.rejects(
    preparePagesArtifacts(fixture.options),
    /content-addressed card corpus hash mismatch/,
  );
  assert.deepEqual(await snapshotTree(fixture.inputDir), before);
  await assert.rejects(readFile(fixture.outputDir), { code: "ENOENT" });
});

test("prepares the Pages shell with missing manifest JSON marked unverified absent", async () => {
  const fixture = await createFixture();
  await rm(path.join(fixture.inputDir, DATA_FILES[0]));
  const before = await snapshotTree(fixture.inputDir);
  const report = await preparePagesArtifacts(fixture.options);
  const missing = report.publicObjects.find((object) => object.key === DATA_FILES[0]);
  assert.equal(missing.encodedBytesStatus, "UNVERIFIED_ABSENT");
  assert.equal(missing.sourcePath, null);
  assert.equal(missing.encodedBytes, null);
  assert.equal(missing.encodedBytesSha256, null);
  assert.equal(missing.decodedBytes, null);
  assert.equal(missing.decodedContentSha256, null);
  assert.deepEqual(missing.sourceArtifacts, []);
  assert.ok(report.deploymentReadiness.blockers.some((blocker) => blocker.includes(DATA_FILES[0])));
  assert.deepEqual(await snapshotTree(fixture.inputDir), before);
});

test("prepares a realistic shell-only input and reports absent external JSON evidence", async () => {
  const fixture = await createFixture({
    includeManifestFiles: false,
    includePlainCardData: false,
    includeHashedCardCorpus: false,
  });
  const before = await snapshotTree(fixture.inputDir);
  const report = await preparePagesArtifacts(fixture.options);
  assert.equal(report.artifactPreparation, "PASS");
  assert.deepEqual(report.removedFromPages, []);
  assert.equal(report.offloadedSources.length, 0);
  assert.equal(report.publicObjects.length, DATA_FILES.length + 1);
  assert.ok(report.publicObjects.every((object) => object.encodedBytesStatus === "UNVERIFIED_ABSENT"));
  assert.ok(report.publicObjects.every((object) => object.encodedBytes === null && object.decodedContentSha256 === null));
  assert.ok(report.deploymentReadiness.blockers.some((blocker) => blocker.includes("externally configured JSON objects are absent locally")));
  assert.deepEqual(await snapshotTree(fixture.inputDir), before);
  const outputPaths = new Set((await snapshotTree(fixture.outputDir)).map((entry) => entry.path));
  assert.deepEqual([...outputPaths].sort(), ["_headers", "assets/app.js", "index.html"]);
});

test("refuses to strip present JSON referenced by supported local bundle literals", async (t) => {
  const references = [
    ["bare filename with query", "card-names.json?cache=1"],
    ["relative path with query", "../card-names.json?cache=1"],
    ["relative path normalized at root", "../../card-names.json?cache=1"],
    ["root-relative path", "/card-names.json#local"],
    ["percent-decoded path", "./%63ard-names%2Ejson?raw=1"],
  ];
  for (const [label, localReference] of references) {
    await t.test(label, async () => {
      const fixture = await createFixture({
        bundleOverrides: {
          bundle: [
            ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
            CONFIG.cardDataUrl,
            CONFIG.engineWasmUrl,
            localReference,
          ],
        },
      });
      await assert.rejects(
        preparePagesArtifacts(fixture.options),
        /local JavaScript reference to offloaded artifact card-names\.json/,
      );
      await assert.rejects(readFile(fixture.outputDir), { code: "ENOENT" });
    });
  }
});

test("rejects a missing index.html", async () => {
  const fixture = await createFixture();
  await rm(path.join(fixture.inputDir, "index.html"));
  await assert.rejects(preparePagesArtifacts(fixture.options), /required build input is missing: index\.html/);
});

test("preserves the DATA_BASE_URL path prefix using Vite append semantics", async () => {
  const prefix = "https://assets.example.test/tenant/releases/2026-q4";
  const fixture = await createFixture({
    bundleOverrides: {
      bundle: [
        ...DATA_FILES.map((filename) => `${prefix}/${filename}`),
        CONFIG.cardDataUrl,
        CONFIG.engineWasmUrl,
      ],
      options: { dataBaseUrl: prefix },
    },
  });
  const report = await preparePagesArtifacts(fixture.options);
  const cardNamesObject = report.publicObjects.find((object) => object.key === "card-names.json");
  assert.equal(cardNamesObject.publicUrl, `${prefix}/card-names.json`);
  assert.equal(report.bundleEvidence.DATA_BASE_URL.status, "MATCH");
});

test("rejects observable build URL mismatches instead of rewriting bundles", async () => {
  const oldBase = "https://assets.example.test/old-release";
  const fixture = await createFixture({
    bundleOverrides: {
      bundle: [
        ...DATA_FILES.map((filename) => `${oldBase}/${filename}`),
        CONFIG.cardDataUrl,
        CONFIG.engineWasmUrl,
      ],
    },
  });
  const sourceBefore = await snapshotTree(fixture.inputDir);
  await assert.rejects(preparePagesArtifacts(fixture.options), /observable DATA_BASE_URL bundle mismatch/);
  assert.deepEqual(await snapshotTree(fixture.inputDir), sourceBefore);
  await assert.rejects(readFile(fixture.outputDir), { code: "ENOENT" });
});

test("rejects a mismatched CARD_DATA_URL and ENGINE_WASM_URL when each is observable", async (t) => {
  await t.test("card URL", async () => {
    const oldCardUrl = `${DATA_BASE_URL}/card-data-0000000000000000.json`;
    const fixture = await createFixture({
      bundleOverrides: {
        bundle: [
          ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
          oldCardUrl,
          CONFIG.engineWasmUrl,
        ],
      },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /observable CARD_DATA_URL bundle mismatch/);
  });

  await t.test("engine URL", async () => {
    const oldEngineUrl = "https://assets.example.test/wasm/engine_wasm_bg-0000000000000000.wasm";
    const fixture = await createFixture({
      bundleOverrides: {
        bundle: [
          ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
          CONFIG.cardDataUrl,
          oldEngineUrl,
        ],
      },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /observable ENGINE_WASM_URL bundle mismatch/);
  });
});

test("marks correspondence unproven when an external engine URL is absent from bundles", async () => {
  const fixture = await createFixture({
    bundleOverrides: {
      bundle: [
        ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
        CONFIG.cardDataUrl,
      ],
    },
  });
  const report = await preparePagesArtifacts(fixture.options);
  assert.equal(report.configuration.callerAttestation.buildUsedTheseUrlValues, true);
  assert.equal(report.bundleEvidence.ENGINE_WASM_URL.status, "UNPROVEN");
  assert.equal(report.wasm.configuredEngineObject.bytes, null);
  assert.equal(report.wasm.configuredEngineObject.sha256, null);
  assert.equal(report.wasm.configuredEngineObject.byteVerification, "UNVERIFIED_ABSENT");
  assert.equal(report.deploymentReadiness.status, "BLOCKED");
});

test("records caller build attestation separately from observable evidence", async () => {
  const fixture = await createFixture({ bundleOverrides: { options: { attestBuildUrls: false } } });
  const report = await preparePagesArtifacts(fixture.options);
  assert.equal(report.configuration.callerAttestation.buildUsedTheseUrlValues, false);
  assert.equal(report.bundleEvidence.DATA_BASE_URL.status, "MATCH");
  assert.equal(report.bundleEvidence.CARD_DATA_URL.status, "MATCH");
  assert.equal(report.bundleEvidence.ENGINE_WASM_URL.status, "MATCH");
  assert.equal(report.deploymentReadiness.status, "BLOCKED");
  assert.ok(report.deploymentReadiness.blockers.some((blocker) => blocker.includes("did not attest")));
});

test("maps Brotli source bytes to the unsuffixed public JSON key", async () => {
  const fixture = await createFixture();
  const filename = "card-names.json";
  const jsonBytes = await readFile(path.join(fixture.inputDir, filename));
  const brotliBytes = brotliCompressSync(jsonBytes);
  await writeFile(path.join(fixture.inputDir, `${filename}.br`), brotliBytes);

  const report = await preparePagesArtifacts(fixture.options);
  const object = report.publicObjects.find((entry) => entry.key === filename);
  assert.equal(object.publicUrl, `${DATA_BASE_URL}/${filename}`);
  assert.equal(object.publicUrl.endsWith(".br"), false);
  assert.equal(object.sourceRepresentation, "brotli");
  assert.equal(object.encodedBytesSha256, createHash("sha256").update(brotliBytes).digest("hex"));
  assert.equal(object.decodedContentSha256, createHash("sha256").update(jsonBytes).digest("hex"));
  assert.equal(object.sourceArtifacts.length, 2);
  await assert.rejects(readFile(path.join(fixture.outputDir, `${filename}.br`)), { code: "ENOENT" });
});

test("accepts a valid compressed-only manifest source without inventing identity bytes", async () => {
  const fixture = await createFixture();
  const filename = "card-names.json";
  const identityPath = path.join(fixture.inputDir, filename);
  const jsonBytes = await readFile(identityPath);
  const brotliBytes = brotliCompressSync(jsonBytes);
  await writeFile(`${identityPath}.br`, brotliBytes);
  await rm(identityPath);

  const report = await preparePagesArtifacts(fixture.options);
  const object = report.publicObjects.find((entry) => entry.key === filename);
  assert.equal(object.sourcePath, `${filename}.br`);
  assert.equal(object.sourceArtifacts.length, 1);
  assert.equal(object.sourceArtifacts[0].representation, "brotli");
  assert.equal(object.decodedContentSha256, createHash("sha256").update(jsonBytes).digest("hex"));
});

test("recognizes and verifies a compressed-only content-addressed card corpus", async () => {
  const fixture = await createFixture();
  const identityPath = path.join(fixture.inputDir, CARD_FILENAME);
  const compressed = brotliCompressSync(CARD_BYTES);
  await writeFile(`${identityPath}.br`, compressed);
  await rm(identityPath);

  const report = await preparePagesArtifacts(fixture.options);
  const object = report.publicObjects.find((entry) => entry.key === CARD_FILENAME);
  assert.equal(object.sourcePath, `${CARD_FILENAME}.br`);
  assert.equal(object.publicUrl, CONFIG.cardDataUrl);
  assert.equal(object.sourceRepresentation, "brotli");
  assert.equal(object.sourceArtifacts.length, 1);
  assert.equal(object.decodedContentSha256, createHash("sha256").update(CARD_BYTES).digest("hex"));
  await assert.rejects(readFile(path.join(fixture.outputDir, `${CARD_FILENAME}.br`)), { code: "ENOENT" });
});

test("rejects invalid Brotli and companions with different decoded content", async (t) => {
  await t.test("invalid bytes", async () => {
    const fixture = await createFixture();
    await writeFile(path.join(fixture.inputDir, "card-names.json.br"), "not brotli");
    await assert.rejects(preparePagesArtifacts(fixture.options), /invalid Brotli source card-names\.json\.br/);
  });
  await t.test("different JSON source", async () => {
    const fixture = await createFixture();
    await writeFile(
      path.join(fixture.inputDir, "card-names.json.br"),
      brotliCompressSync(Buffer.from('{"different":true}')),
    );
    await assert.rejects(preparePagesArtifacts(fixture.options), /Brotli companion does not decode to card-names\.json/);
  });
});

test("preserves and hashes local WASM and rejects missing local references", async (t) => {
  await t.test("local reference and local engine provenance", async () => {
    const fixture = await createFixture({ localWasmReference: true, includeConfiguredEngine: true });
    const report = await preparePagesArtifacts(fixture.options);
    assert.ok(report.wasm.localFiles.some((file) => file.path === "assets/draft_wasm_bg.wasm"));
    assert.ok(report.wasm.localReferences.some((reference) => reference.target === "assets/draft_wasm_bg.wasm"));
    assert.equal(report.wasm.configuredEngineObject.byteVerification, "VERIFIED_LOCAL");
    assert.deepEqual(
      await readFile(path.join(fixture.outputDir, "assets", "draft_wasm_bg.wasm")),
      Buffer.from([0, 97, 115, 109]),
    );
    assert.deepEqual(
      await readFile(path.join(fixture.outputDir, `engine_wasm_bg-${ENGINE_HASH}.wasm`)),
      ENGINE_BYTES,
    );
  });

  await t.test("missing local reference", async () => {
    const fixture = await createFixture({
      bundleOverrides: {
        bundle: [
          ...DATA_FILES.map((filename) => `${DATA_BASE_URL}/${filename}`),
          CONFIG.cardDataUrl,
          CONFIG.engineWasmUrl,
          "./missing.wasm",
        ],
      },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /referenced local WASM is missing/);
  });
});

test("copies pinned _headers, keeps the SPA entry, and does not create a 404 page", async () => {
  const fixture = await createFixture();
  const report = await preparePagesArtifacts(fixture.options);
  const expectedHeaders = await readFile(new URL("../client/deploy/cloudflare-pages/_headers", import.meta.url));
  assert.deepEqual(await readFile(path.join(fixture.outputDir, "_headers")), expectedHeaders);
  assert.equal((await readFile(path.join(fixture.outputDir, "index.html"))).length > 0, true);
  await assert.rejects(readFile(path.join(fixture.outputDir, "404.html")), { code: "ENOENT" });
  assert.deepEqual(report.spa, {
    indexHtml: "PRESERVED",
    topLevel404: "ABSENT",
    pagesDefaultSpaFallback: "EXPECTED_WHEN_TOP_LEVEL_404_IS_ABSENT",
  });
});

test("omits only a byte-identical historical 404.html and rejects custom routing", async (t) => {
  await t.test("identical fallback", async () => {
    const fixture = await createFixture();
    const index = await readFile(path.join(fixture.inputDir, "index.html"));
    await writeFile(path.join(fixture.inputDir, "404.html"), index);
    const report = await preparePagesArtifacts(fixture.options);
    assert.equal(report.spa.topLevel404, "OMITTED_IDENTICAL_TO_INDEX");
    await assert.rejects(readFile(path.join(fixture.outputDir, "404.html")), { code: "ENOENT" });
  });
  await t.test("custom fallback", async () => {
    const fixture = await createFixture();
    await writeFile(path.join(fixture.inputDir, "404.html"), "explicit custom route\n");
    await assert.rejects(preparePagesArtifacts(fixture.options), /explicit routing decision is required/);
    await assert.rejects(readFile(fixture.outputDir), { code: "ENOENT" });
  });
});

test("rejects symlinks in input trees, input roots, and output parents", async (t) => {
  await t.test("symlink entry", async () => {
    const fixture = await createFixture();
    await symlink(path.join(fixture.inputDir, "index.html"), path.join(fixture.inputDir, "alias.html"));
    await assert.rejects(preparePagesArtifacts(fixture.options), /build tree contains a symlink/);
  });
  await t.test("symlink input root", async () => {
    const fixture = await createFixture();
    const alias = path.join(fixture.root, "input-link");
    await symlink(fixture.inputDir, alias);
    await assert.rejects(
      preparePagesArtifacts({ ...fixture.options, inputDir: alias }),
      /input path contains a symlink/,
    );
  });
  await t.test("symlink output parent", async () => {
    const fixture = await createFixture();
    const targetParent = path.join(fixture.root, "safe-parent");
    await mkdir(targetParent);
    const linkParent = path.join(fixture.root, "linked-parent");
    await symlink(targetParent, linkParent);
    await assert.rejects(
      preparePagesArtifacts({ ...fixture.options, outputDir: path.join(linkParent, "output") }),
      /output parent path contains a symlink/,
    );
  });
});

test("rejects input/output nesting in both path directions", async (t) => {
  await t.test("output inside input", async () => {
    const fixture = await createFixture();
    const nested = path.join(fixture.inputDir, "nested-output");
    await mkdir(nested);
    await assert.rejects(
      preparePagesArtifacts({ ...fixture.options, outputDir: path.join(nested, "dist") }),
      /input and output directories must be separate/,
    );
  });
  await t.test("same path", async () => {
    const fixture = await createFixture();
    await assert.rejects(
      preparePagesArtifacts({ ...fixture.options, outputDir: fixture.inputDir }),
      /output destination already exists/,
    );
  });
});

test("refuses to overwrite an existing output destination", async () => {
  const fixture = await createFixture();
  await mkdir(fixture.outputDir);
  await writeFile(path.join(fixture.outputDir, "sentinel.txt"), "leave untouched");
  await assert.rejects(preparePagesArtifacts(fixture.options), /output destination already exists/);
  assert.equal(await readFile(path.join(fixture.outputDir, "sentinel.txt"), "utf8"), "leave untouched");
});

test("enforces the inclusive 25 MiB boundary on the copied final tree", async (t) => {
  await t.test("exact limit accepted", async () => {
    const fixture = await createFixture();
    const limitFile = path.join(fixture.inputDir, "assets", "limit.bin");
    await writeFile(limitFile, Buffer.alloc(0));
    await truncate(limitFile, PAGES_MAX_FILE_BYTES);
    const report = await preparePagesArtifacts(fixture.options);
    assert.equal(report.pagesOutput.largestFile.path, "assets/limit.bin");
    assert.equal(report.pagesOutput.largestFile.bytes, 26_214_400);
  });
  await t.test("one byte over rejected", async () => {
    const fixture = await createFixture();
    const limitFile = path.join(fixture.inputDir, "assets", "limit.bin");
    await writeFile(limitFile, Buffer.alloc(0));
    await truncate(limitFile, PAGES_MAX_FILE_BYTES + 1);
    await assert.rejects(
      preparePagesArtifacts(fixture.options),
      /limit\.bin is 26214401 bytes; limit is 26214400/,
    );
    await assert.rejects(readFile(fixture.outputDir), { code: "ENOENT" });
  });
});

test("enforces exactly 20,000 files, including the file-count boundary", () => {
  const atLimit = Array.from({ length: PAGES_FREE_MAX_FILES }, (_, index) => ({
    path: `file-${index}.bin`,
    size: 0,
  }));
  assert.equal(validatePagesLimits(atLimit).fileCount, 20_000);
  assert.throws(
    () => validatePagesLimits([...atLimit, { path: "one-too-many.bin", size: 0 }]),
    /20001 files; limit is 20000/,
  );
});

test("produces path-independent deterministic reports and preserves source inputs", async () => {
  const first = await createFixture();
  const firstBefore = await snapshotTree(first.inputDir);
  const firstReport = await preparePagesArtifacts(first.options);
  const firstAfter = await snapshotTree(first.inputDir);
  assert.deepEqual(firstAfter, firstBefore);

  const secondRoot = await mkdtemp(path.join(await realpath(os.tmpdir()), "pages-prep-other-path-"));
  roots.add(secondRoot);
  const second = await createFixture({ baseDirectory: secondRoot });
  const secondReport = await preparePagesArtifacts(second.options);
  assert.notEqual(first.inputDir, second.inputDir);
  assert.notEqual(first.outputDir, second.outputDir);
  assert.deepEqual(secondReport, firstReport);
  assert.equal(JSON.stringify(firstReport).includes(first.inputDir), false);
  assert.equal(JSON.stringify(firstReport).includes(first.outputDir), false);
});

test("CLI round-trips bounded caller provenance into the same deterministic report as the API", async () => {
  const artifactProvenance = {
    sourceRevision: "a".repeat(40),
    sourceArtifactSha256: "b".repeat(64),
    externalEngineWasmSha256: "c".repeat(64),
    buildRunId: "release-2026-10-01.7",
  };
  const fixture = await createFixture();
  const expected = await preparePagesArtifacts({ ...fixture.options, artifactProvenance });
  await rm(fixture.outputDir, { recursive: true, force: true });
  const cliReport = execFileSync(
    process.execPath,
    [
      path.join(REPOSITORY_ROOT, "scripts", "prepare-cloudflare-pages.mjs"),
      "--input",
      fixture.inputDir,
      "--output",
      fixture.outputDir,
      "--data-base-url",
      CONFIG.dataBaseUrl,
      "--card-data-url",
      CONFIG.cardDataUrl,
      "--engine-wasm-url",
      CONFIG.engineWasmUrl,
      "--artifact-provenance-json",
      JSON.stringify(artifactProvenance),
      "--attest-build-urls",
    ],
    { cwd: REPOSITORY_ROOT, encoding: "utf8" },
  );
  assert.deepEqual(JSON.parse(cliReport), expected);
  assert.deepEqual(expected.callerSuppliedArtifactProvenance, {
    status: "CALLER_SUPPLIED_NOT_LOCALLY_VERIFIED",
    ...artifactProvenance,
  });
  assert.equal(expected.wasm.configuredEngineObject.sha256, null);
  assert.equal(expected.wasm.configuredEngineObject.byteVerification, "UNVERIFIED_ABSENT");
});

test("keeps local checks and caller provenance separate and rejects unsafe provenance fields", async (t) => {
  await t.test("empty provenance input is explicitly absent", async () => {
    const fixture = await createFixture();
    const report = await preparePagesArtifacts(fixture.options);
    assert.deepEqual(report.callerSuppliedArtifactProvenance, { status: "NOT_SUPPLIED" });
  });

  await t.test("unknown fields are not copied into the report", async () => {
    const fixture = await createFixture();
    await assert.rejects(
      preparePagesArtifacts({
        ...fixture.options,
        artifactProvenance: { sourceRevision: "a".repeat(40), operatorNote: "private text" },
      }),
      /no additional fields/,
    );
  });

  await t.test("invalid identifiers and credential-like run IDs are rejected", async () => {
    const fixture = await createFixture();
    await assert.rejects(
      preparePagesArtifacts({ ...fixture.options, artifactProvenance: { sourceRevision: "not-a-revision" } }),
      /40- or 64-character lowercase Git object ID/,
    );
    await assert.rejects(
      preparePagesArtifacts({
        ...fixture.options,
        artifactProvenance: { buildRunId: `AKIA${"A".repeat(16)}` },
      }),
      /safe 1-64 character identifier/,
    );
  });

  await t.test("CLI bounds provenance JSON before parsing it", async () => {
    const fixture = await createFixture();
    const oversizedJson = `{"sourceRevision":"${"a".repeat(40)}"}${" ".repeat(513)}`;
    assert.throws(
      () => execFileSync(
        process.execPath,
        [
          path.join(REPOSITORY_ROOT, "scripts", "prepare-cloudflare-pages.mjs"),
          "--input", fixture.inputDir,
          "--output", fixture.outputDir,
          "--data-base-url", CONFIG.dataBaseUrl,
          "--card-data-url", CONFIG.cardDataUrl,
          "--engine-wasm-url", CONFIG.engineWasmUrl,
          "--artifact-provenance-json", oversizedJson,
        ],
        { cwd: REPOSITORY_ROOT, encoding: "utf8" },
      ),
      (error) => String(error.stderr).includes("--artifact-provenance-json exceeds 512 UTF-8 bytes"),
    );
  });
});

test("rejects secret-bearing and non-HTTPS URL configuration", async (t) => {
  await t.test("Stripe-style secret in URL path", async () => {
    const fixture = await createFixture({
      bundleOverrides: {
        options: {
          dataBaseUrl: "https://assets.example.test/releases/sk_live_12345678901234567890",
        },
      },
    });
    await assertCredentialRejectedWithoutEcho(
      preparePagesArtifacts(fixture.options),
      "sk_live_12345678901234567890",
    );
  });
  await t.test("GitHub fine-grained token in URL path", async () => {
    const fixture = await createFixture({
      bundleOverrides: {
        options: {
          dataBaseUrl:
            "https://assets.example.test/releases/github_pat_11AA22BB33CC44DD55EE66FF77GG88HH99II00JJ",
        },
      },
    });
    await assertCredentialRejectedWithoutEcho(
      preparePagesArtifacts(fixture.options),
      "github_pat_11AA22BB33CC44DD55EE66FF77GG88HH99II00JJ",
    );
  });
  await t.test("percent-encoded Stripe-style secret in URL path", async () => {
    const fixture = await createFixture({
      bundleOverrides: {
        options: {
          dataBaseUrl: "https://assets.example.test/releases/%73k_live_12345678901234567890",
        },
      },
    });
    await assertCredentialRejectedWithoutEcho(
      preparePagesArtifacts(fixture.options),
      "sk_live_12345678901234567890",
    );
  });
  await t.test("percent-encoded GitHub fine-grained token in URL path", async () => {
    const fixture = await createFixture({
      bundleOverrides: {
        options: {
          dataBaseUrl:
            "https://assets.example.test/releases/%67ithub_pat_11AA22BB33CC44DD55EE66FF77GG88HH99II00JJ",
        },
      },
    });
    await assertCredentialRejectedWithoutEcho(
      preparePagesArtifacts(fixture.options),
      "github_pat_11AA22BB33CC44DD55EE66FF77GG88HH99II00JJ",
    );
  });
  await t.test("query string", async () => {
    const fixture = await createFixture({
      bundleOverrides: { options: { dataBaseUrl: `${DATA_BASE_URL}?token=hidden` } },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /query string or fragment/);
  });
  await t.test("URL credentials", async () => {
    const fixture = await createFixture({
      bundleOverrides: { options: { cardDataUrl: `https://user:pass@assets.example.test/${CARD_FILENAME}` } },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /URL credentials/);
  });
  await t.test("non-HTTPS", async () => {
    const fixture = await createFixture({
      bundleOverrides: { options: { engineWasmUrl: CONFIG.engineWasmUrl.replace("https:", "http:") } },
    });
    await assert.rejects(preparePagesArtifacts(fixture.options), /ENGINE_WASM_URL must use https/);
  });
});
