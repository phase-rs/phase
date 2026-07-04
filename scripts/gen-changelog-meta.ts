#!/usr/bin/env bun
/**
 * Generates client/public/changelog-meta.json from changelog.json.
 *
 * The meta file is the tiny, constant-size payload every client fetches on load
 * to decide whether to show the "What's New" unread dot — it must never grow
 * with the changelog. It is GENERATED, never hand-edited: this script is the
 * single authority for its contents.
 *
 * Run with no args to (re)write the meta file. Run with `--check` for the CI
 * parity gate: it regenerates in memory and fails if the committed meta drifts
 * from what changelog.json implies (i.e. someone edited changelog.json without
 * regenerating, or hand-edited the meta).
 *
 * While here it also asserts the changelog invariants the frontend relies on:
 * entries newest-first (strictly descending id) with unique ids, so
 * `entries[0].id` is always the max and the unread watermark is monotonic.
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

interface ChangelogEntry {
  id: number;
}
interface Changelog {
  entries: ChangelogEntry[];
}

const ROOT = path.resolve(import.meta.dir, "..");
const CHANGELOG_PATH = path.join(ROOT, "client/public/changelog.json");
const META_PATH = path.join(ROOT, "client/public/changelog-meta.json");

function computeMeta(): string {
  const { entries } = JSON.parse(
    readFileSync(CHANGELOG_PATH, "utf-8"),
  ) as Changelog;

  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error("changelog.json must contain a non-empty `entries` array");
  }

  const seen = new Set<number>();
  for (let i = 0; i < entries.length; i++) {
    const { id } = entries[i];
    if (!Number.isInteger(id)) {
      throw new Error(`entry ${i} has a non-integer id: ${JSON.stringify(id)}`);
    }
    if (seen.has(id)) throw new Error(`duplicate changelog id: ${id}`);
    seen.add(id);
    if (i > 0 && entries[i - 1].id <= id) {
      throw new Error(
        `entries must be sorted by descending id (newest first); ` +
          `id ${entries[i - 1].id} is not greater than following id ${id}`,
      );
    }
  }

  // Pretty-print to match the committed file's formatting exactly so the
  // --check byte comparison is stable.
  return `${JSON.stringify({ latestId: entries[0].id }, null, 2)}\n`;
}

const check = process.argv.includes("--check");
const meta = computeMeta();

if (check) {
  const committed = readFileSync(META_PATH, "utf-8");
  if (committed !== meta) {
    console.error(
      "changelog-meta.json is out of date with changelog.json.\n" +
        "Run `bun scripts/gen-changelog-meta.ts` and commit the result.",
    );
    process.exit(1);
  }
  console.log("changelog-meta.json is up to date.");
} else {
  writeFileSync(META_PATH, meta);
  console.log(`Wrote ${path.relative(ROOT, META_PATH)} (latestId from changelog.json).`);
}
