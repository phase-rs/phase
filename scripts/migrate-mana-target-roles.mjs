#!/usr/bin/env node
/**
 * Migrate `Effect::Mana.target` in a generated card-data fixture from the legacy
 * bare `TargetFilter` encoding to the `ManaTargetRole` encoding introduced by
 * PR #6056.
 *
 * ---------------------------------------------------------------------------
 * WHY THIS SCRIPT EXISTS (read this before running it)
 * ---------------------------------------------------------------------------
 * PR #6056 changed the type of `Effect::Mana.target`:
 *
 *     BEFORE:  target: Option<TargetFilter>
 *     AFTER:   target: Option<ManaTargetRole>
 *
 * CR 601.2c requires an independent choice per instance of the word "target".
 * A mana sentence can name two of them, with different jobs:
 *
 *   - RECIPIENT    — whose mana pool receives the mana (CR 106.4).
 *                    "Target player adds that much {C}."  (Jetfire)
 *   - COUNT SOURCE — the player a production-count quantity reads.
 *                    "Add {R} for each card in target opponent's hand."
 *                    (Jeska's Will, Carpet of Flowers)
 *
 * The old single field served both, so the resolver had to GUESS the role from
 * the production's quantity shape. That guess is the bug the PR removes; the
 * role is now stamped at parse time from grammar.
 *
 * `crates/engine/tests/fixtures/integration_cards.json` is GENERATED (a curated
 * subset of `client/public/card-data.json`) and is git-tracked as a single
 * minified line. It therefore carries the old encoding and must be migrated
 * whenever it is regenerated on a checkout that has PR #6056's engine changes.
 *
 * ---------------------------------------------------------------------------
 * WHY THE MAPPING IS KEYED BY CARD NAME AND NOT DERIVED FROM THE FILTER
 * ---------------------------------------------------------------------------
 * This is the single most important property of this script. The role is NOT
 * recoverable from the serialized filter. Two entries in the fixture prove it —
 * identical `Typed` shapes, opposite roles:
 *
 *   carpet of flowers    Typed{controller:"Opponent"}              -> CountSource
 *   spectral searchlight Typed{controller:{ChosenPlayer:{index:0}}} -> Recipient
 *
 * Any attempt to infer the role from the filter (or from the production's
 * quantity shape) reintroduces exactly the defect PR #6056 exists to remove,
 * and a maintainer explicitly rejected that approach in review. The parser is
 * the real authority — it knows which grammatical position a filter came from.
 * This table is a faithful transcription of what the parser produces for these
 * 11 cards; it is not a heuristic.
 *
 * ---------------------------------------------------------------------------
 * HOW TO USE IT
 * ---------------------------------------------------------------------------
 *   1. Regenerate card data and the fixture through the normal project path:
 *          ./scripts/gen-card-data.sh
 *          python3 scripts/gen-test-fixture.py
 *   2. Apply this migration:
 *          node scripts/migrate-mana-target-roles.mjs
 *   3. Verify:
 *          cargo test -p engine --test integration
 *      In particular `mana_role_fixture_migration` asserts Carpet of Flowers is
 *      a CountSource and Belbe is a Recipient — it is the canary for a mapping
 *      that silently flipped.
 *
 * Optional first argument overrides the fixture path (useful for dry runs on a
 * copy). `--check` reports what WOULD change and writes nothing.
 *
 * The script is IDEMPOTENT: entries already in role form are left untouched, so
 * re-running it after a partial migration is safe.
 *
 * ---------------------------------------------------------------------------
 * THE SAFETY PROPERTY THAT MATTERS MOST
 * ---------------------------------------------------------------------------
 * If regeneration introduces a mana-target card that is NOT in the table below,
 * this script FAILS LOUDLY rather than guessing. Do not "fix" that by inventing
 * a role or by falling back to a shape heuristic. Determine the correct role
 * from the card's Oracle text:
 *
 *   - the targeted player RECEIVES the mana        -> Recipient
 *   - the targeted player only supplies the AMOUNT -> CountSource
 *   - a sentence that does both                    -> Both { recipient, count_source }
 *
 * ...then add it here with a comment naming the clause that decided it.
 *
 * As of PR #6056 the set is stable at exactly 11 entries: it was verified to be
 * identical (name-for-name) between the PR branch and a later independent
 * regeneration of `main` that added 139 cards and changed 1,278 shared entries.
 * So a regeneration adding new mana-target cards is possible but has not
 * happened yet — treat it as a real signal, not noise.
 */

import { readFileSync, writeFileSync } from "node:fs";

const DEFAULT_FIXTURE = "crates/engine/tests/fixtures/integration_cards.json";

/**
 * Card name (as keyed in the fixture — lowercased) -> role variant.
 *
 * Every entry below is a RECIPIENT except Carpet of Flowers. The recipients
 * split into two grammatical families, both of which are context-refs (they
 * surface no target slot) — which is another reason shape-based inference fails:
 *
 *   - subject-led / phase-scoped ("that player adds", "the active player adds")
 *   - replacement-style land auras ("...adds an additional {G}")
 */
const ROLE_BY_CARD = new Map([
  // "Whenever a player taps a Forest for mana, that player adds..." — the
  // targeted/scoped player RECEIVES the mana.
  ["belbe, corrupted observer", "Recipient"],
  ["blinkmoth urn", "Recipient"],
  ["bubbling muck", "Recipient"],
  ["high tide", "Recipient"],
  ["mana flare", "Recipient"],
  // Land auras: "Enchanted land ... adds an additional {G}" — the land's
  // controller RECEIVES the extra mana.
  ["fertile ground", "Recipient"],
  ["shimmerwilds growth", "Recipient"],
  ["utopia sprawl", "Recipient"],
  ["wild growth", "Recipient"],
  // "Choose a player. That player adds one mana of any color they choose."
  // The CHOSEN player RECEIVES it. Note this is a `Typed` filter, same outer
  // shape as Carpet of Flowers below but the opposite role.
  ["spectral searchlight", "Recipient"],
  // "...add {U} for each Island target opponent controls." The opponent only
  // supplies the AMOUNT; the controller receives the mana. THE canary entry —
  // if a careless bulk rewrite flips anything, it flips this one.
  ["carpet of flowers", "CountSource"],
  // --- Entries below appear only in persisted game-state dumps (`--state`),
  // --- not in the curated card fixture. Verified against Scryfall Oracle text.
  // "Whenever enchanted land is tapped for mana, its controller adds an
  // additional {G}." — subject-led anaphoric recipient (ParentTargetController).
  ["wolfwillow haven", "Recipient"],
  // "You add {B}{B} and draw a card." — subject-led "you" recipient (Controller).
  ["priest of forgotten gods", "Recipient"],
  // "Add {R} for each card in target opponent's hand." — the Jeska's Will
  // clause verbatim: the opponent supplies only the AMOUNT.
  ["rousing refrain", "CountSource"],
]);

/**
 * Cards that appear only in persisted game-state dumps (`--state` mode), never
 * in the curated card fixture. Excluded from fixture mode's drop-detection
 * count so the "expected N entries" guard keeps catching genuine renames/drops
 * of the curated 11.
 */
const STATE_ONLY_CARDS = new Set([
  "wolfwillow haven",
  "priest of forgotten gods",
  "rousing refrain",
]);

/** Field name carrying the filter inside each role variant. */
const FIELD_BY_ROLE = {
  Recipient: "recipient",
  CountSource: "count_source",
};

const args = process.argv.slice(2);
const checkOnly = args.includes("--check");
const stateMode = args.includes("--state");
const fixturePath = args.find((a) => !a.startsWith("--")) ?? DEFAULT_FIXTURE;

// ---------------------------------------------------------------------------
// `--state <file.json.gz>`: migrate a PERSISTED GAME-STATE dump instead of the
// curated card fixture. Several test suites (kilo_live_offer_from_real_dump,
// combo_infinite_pile, sprout_inalla_realistic_offer) load gzipped `GameState`
// dumps whose objects carry parsed abilities in the pre-role encoding. A state
// deserializes through typed serde BEFORE any restore-time migration hook can
// run, so the dump itself must carry the role encoding.
//
// Role attribution uses the nearest enclosing object `name` — the game object
// that carries the ability — resolved through the SAME name-keyed table.
// Unknown names fail loudly, exactly as in fixture mode: a state that names a
// mana-target card missing from the table needs a human role decision from
// that card's Oracle text, never an inference.
//
// NOTE for production saves: this migrates TEST dumps. If real persisted games
// can carry pre-role Mana targets across an engine upgrade, the restore path
// needs a JSON-level migration before typed deserialization — that is a
// maintainer decision, not something this script papers over.
// ---------------------------------------------------------------------------
if (stateMode) {
  const zlib = await import("node:zlib");
  // Local copy: the shared `isRole` is declared later in the file (after the
  // fixture-mode load) and consts are TDZ-scoped.
  const isRole = (target) =>
    target !== null && typeof target === "object" && typeof target.role === "string";
  const raw = readFileSync(fixturePath);
  const text = zlib.gunzipSync(raw).toString("utf8");
  const doc = JSON.parse(text);

  const migrated = [];
  const alreadyRole = [];
  const unknown = [];
  (function walk(node, owner) {
    if (node === null || typeof node !== "object") return;
    if (Array.isArray(node)) {
      node.forEach((x) => walk(x, owner));
      return;
    }
    const nextOwner =
      typeof node.name === "string" && node.name.length > 0 ? node.name : owner;
    if (node.type === "Mana" && node.target != null) {
      if (isRole(node.target)) {
        alreadyRole.push(nextOwner);
      } else {
        const role = ROLE_BY_CARD.get(nextOwner.toLowerCase());
        if (role === undefined) {
          unknown.push(nextOwner);
        } else {
          node.target = { role, [FIELD_BY_ROLE[role]]: node.target };
          migrated.push(`${nextOwner} -> ${role}`);
        }
      }
    }
    for (const v of Object.values(node)) walk(v, nextOwner);
  })(doc, "?");

  if (unknown.length > 0) {
    console.error(
      `\nERROR: state dump carries Effect::Mana target(s) on unmapped card(s):\n` +
        [...new Set(unknown)].map((n) => `  - ${n}`).join("\n") +
        `\nDecide each role from the card's Oracle text and add it to ROLE_BY_CARD.\n`,
    );
    process.exit(1);
  }
  console.log(
    `${checkOnly ? "[--check] would migrate" : "migrated"} ${migrated.length} state entr` +
      `${migrated.length === 1 ? "y" : "ies"} in ${fixturePath}` +
      (alreadyRole.length ? ` (${alreadyRole.length} already in role form)` : ""),
  );
  migrated.forEach((l) => console.log(`  ${l}`));
  if (!checkOnly && migrated.length > 0) {
    writeFileSync(fixturePath, zlib.gzipSync(JSON.stringify(doc)));
    console.log(`rewrote ${fixturePath} (gzipped, minified)`);
  }
  process.exit(0);
}

const raw = readFileSync(fixturePath, "utf8");
const db = JSON.parse(raw);
const cards = db.cards ?? db;

/** True when `target` is already a `ManaTargetRole` rather than a bare filter. */
const isRole = (target) =>
  target !== null && typeof target === "object" && typeof target.role === "string";

const migrated = [];
const alreadyRole = [];
const unknown = [];

for (const [name, card] of Object.entries(cards)) {
  // Walk the whole card: `Effect::Mana` can appear nested inside triggers,
  // replacement definitions, sub-ability chains, and modal branches, so a
  // shallow scan of top-level abilities would miss real entries.
  (function walk(node) {
    if (node === null || typeof node !== "object") return;
    if (Array.isArray(node)) {
      node.forEach(walk);
      return;
    }
    if (node.type === "Mana" && node.target != null) {
      if (isRole(node.target)) {
        alreadyRole.push(name);
      } else {
        const role = ROLE_BY_CARD.get(name);
        if (role === undefined) {
          unknown.push(name);
        } else {
          // Wrap the existing filter verbatim — the filter itself is unchanged
          // by the migration, only the role envelope around it is new.
          node.target = { role, [FIELD_BY_ROLE[role]]: node.target };
          migrated.push(`${name} -> ${role}`);
        }
      }
    }
    Object.values(node).forEach(walk);
  })(card);
}

// ---------------------------------------------------------------------------
// Fail loudly rather than guessing. See "THE SAFETY PROPERTY THAT MATTERS MOST".
// ---------------------------------------------------------------------------
if (unknown.length > 0) {
  console.error(
    `\nERROR: ${unknown.length} card(s) carry an Effect::Mana target with no role mapping:\n` +
      unknown.map((n) => `  - ${n}`).join("\n") +
      `\n\nThe role is NOT derivable from the filter shape (carpet of flowers and\n` +
      `spectral searchlight are both Typed with OPPOSITE roles). Read each card's\n` +
      `Oracle text and decide:\n` +
      `  targeted player RECEIVES the mana        -> Recipient\n` +
      `  targeted player supplies only the AMOUNT -> CountSource\n` +
      `  a sentence doing both                    -> Both { recipient, count_source }\n` +
      `Then add it to ROLE_BY_CARD in ${import.meta.url.split("/").pop()} with a\n` +
      `comment naming the clause that decided it. Do NOT infer it.\n`,
  );
  process.exit(1);
}

const expected = ROLE_BY_CARD.size - STATE_ONLY_CARDS.size;
const touched = migrated.length + alreadyRole.length;
if (touched !== expected) {
  console.error(
    `\nERROR: expected ${expected} Effect::Mana target entries, found ${touched}.\n` +
      `A card in ROLE_BY_CARD may have been renamed or dropped from the fixture,\n` +
      `or the fixture's curated card set changed. Reconcile before writing.\n` +
      `  migrated: ${migrated.length}, already in role form: ${alreadyRole.length}\n`,
  );
  process.exit(1);
}

console.log(
  `${checkOnly ? "[--check] would migrate" : "migrated"} ${migrated.length} entr` +
    `${migrated.length === 1 ? "y" : "ies"}` +
    (alreadyRole.length ? ` (${alreadyRole.length} already in role form)` : "") +
    ":",
);
for (const line of migrated.length ? migrated : alreadyRole.map((n) => `${n} (unchanged)`)) {
  console.log(`  ${line}`);
}

if (checkOnly) process.exit(0);
if (migrated.length === 0) {
  console.log("\nNothing to do — fixture is already migrated.");
  process.exit(0);
}

// The fixture is tracked as ONE minified line and is expected to round-trip its
// generator's encoding. Pretty-printing it would turn a 1-line diff into
// hundreds of thousands of lines and make the change unreviewable.
const out = `${JSON.stringify(db)}\n`;
writeFileSync(fixturePath, out);

const lineCount = out.split("\n").length - 1;
if (lineCount !== 1) {
  console.error(`\nERROR: fixture must stay a single minified line, got ${lineCount}.`);
  process.exit(1);
}
console.log(`\nWrote ${fixturePath} (1 line, ${out.length} bytes).`);
console.log("Verify with: cargo test -p engine --test integration");
