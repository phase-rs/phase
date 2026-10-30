# Commander bracket corpus

This directory is a dated, provenance-carrying corpus for the Commander bracket estimator. A fixture's `label` is ground truth supplied by its recorded labeller; `expectations.json` is the engine's current reading and is deliberately separate. Changing an estimator threshold or a curated list may change a golden reading, but it must not silently rewrite the label or its citation.

`rules_derivation` fixtures are designed adversarial examples. They are always excluded from every population rate, even when the engine agrees with them. `owner_declared` and permitted `third_party_published` fixtures may enter population rates unless disputed. The three repo-owned cEDH rows are the entire current population (`n = 3`), so all rates remain report-only and `band_agreement_floor` remains `null`.

Every fixture contains exactly 100 cards including its commander, as required by CR 903.5a. Designed padding uses only basic lands permitted by the commander's colour identity under CR 903.4. These references were verified against `docs/MagicCompRules.txt` at lines 6958 and 6940 respectively on 2026-09-26; they are deck-construction references, not Rust rules annotations.

## Legitimate sources for labelled Commander decklists

This table is the sourcing decision from plan 61-06. It is a policy record, not permission to fetch anything. The corpus build performs no scraping or external queries.

| Source | Verdict | Evidence |
|---|---|---|
| **EDHREC** | **Never, by any method.** Its ToS bars accessing the site *"in order to build a similar or competitive website."* A bracket tool is disqualified by **purpose**, not by method — a hand capture is no safer than a script. | EDHREC ToS, fetched 2026-09-12 (probe evidence). |
| **Archidekt** | **The only source of owner-declared bracket tags, and it is hand-capture-only — and even then the 100-card list may not be committed without written permission.** Its ToS prohibits software *"to generate automated searches, requests, or queries to the Site"* **and** states *"no part of the Site may be copied, reproduced, distributed, republished, downloaded."* Recording a **URL + the owner's stated bracket + a terms-check date** is a citation. Committing the deck's card list into this repo is a reproduction. | Archidekt ToS, fetched 2026-09-12 (probe evidence). |
| **Moxfield** | **Not permitted for bulk.** Its ToS returned HTTP 403 to the probe and is **UNVERIFIED**; there is no documented public API. phase's own `lobby-worker/src/import-deck.ts` calls `api2.moxfield.com/v2/decks/all/<id>` for a **single user-initiated import**, which is a different act from corpus building and is not a precedent for it. | Probe evidence; the importer call site. |
| **cEDH Decklist Database** | **Not a clean bulk source.** Its copyright page cites MIT only for Tabler Icons and states no license on the decklist data; its entries are Moxfield links, so the 100 cards still come from Moxfield. | Probe evidence. |
| **MTGJSON** | **Clean (MIT) and already in phase's pipeline — but supplies no bracket labels.** After the 2025-10-21 decoupling a precon carries no official bracket. MTGJSON precons are therefore an **unlabelled distribution source**, never a labelled row. | `commander-brackets-beta-update-october-21-2025.txt:213-224`; `crates/engine/src/bin/oracle_gen.rs:2032-2150` (`run_decks` writes `client/public/decks.json` with `coveragePct` + `unsupported` per deck). |
| **edhtop16 / Topdeck.gg** | **Ask for written permission first.** Do not plan on it. | Probe recommendation. |
| **Repo-owned** | **Clean, tiny, immediately usable.** `client/src/data/cedhDecks.ts` holds **exactly 3** bundled cEDH decks (`:18` `BUNDLED_CEDH_DECKS`, entries at `:33`, `:97`, `:186`) with `bracket: 5` baked in by this repo's own authors — an **owner declaration**. Their own doc-comment says the curated portion is *"demo-quality (not a real tournament list)"* and the remainder is padded with basics; that caveat is recorded on the fixture. `crates/engine/src/starter_decks.rs` is 60-card constructed, not Commander, and is not a source. | File read 2026-09-12. |
| **phase's own users** | **The only scalable legitimate path.** A consent-recorded submission from a phase user (Discord, lobby) carries the label *and* the permission. It needs a consent note per fixture and nothing else. | — |

There is no legitimately obtainable owner-labelled population large enough to gate a rate today. New population rows require written permission or a consent-recorded user submission. Third-party rows also require a source URL and a contemporaneous terms-check date.

## Deterministic split

After designed rows are removed and remaining ids sorted, a row is held out when its zero-based index modulo 4 equals 3.

Designed rows never occupy a train or held-out slot. Disputed population rows keep their deterministic slot but are excluded from scoring.

## Seed readings

Axis columns are Game Changers / Mass Land Denial / Extra Turns / Efficient Tutors. Citations are the recorded basis for the label, not an explanation retrofitted to the engine output.

| Fixture | Label | Citation | Current engine reading and note |
|---|---:|---|---|
| `designed-001-winter-orb-prison` | Optimized; 0/2/0/0 | *Introducing Commander Brackets Beta*, lines 136 and 138-148. | Optimized; 0/2/0/0. The plan predicted an MLD miss. It is no longer a miss because the landed 2026-02-09 `mass_land_sweepers` + `mass_mana_denial` lists classify both Winter Orb and Blood Moon. |
| `designed-002-aggravated-assault-combat` | Core; 0/0/0/0 | Aggravated Assault Oracle text captured 2026-09-12: the ability creates an additional combat and main phase, not an extra turn. | Core; 0/0/0/0. The plan predicted an extra-turn miss. It is no longer a miss because Aggravated Assault was re-filed from `extra_turns` to the evidence-only `extra_combats` list. |
| `designed-003-chained-extra-turns` | Optimized; 0/0/3/0 | *Introducing Commander Brackets Beta*, line 172. | Core; 0/0/3/0. The predicted band miss remains: the current Extra Turns floor is Core even for three cards intended to be chained. |
| `designed-004-zero-signal` | Core; 0/0/0/0 | *Introducing Commander Brackets Beta*, line 302, plus *Commander Brackets Beta Update*, lines 213-224. | Core; 0/0/0/0. The plan predicted an Exhibition/Core band miss. It is no longer a miss because the landed estimator base floor is Core. |
| `designed-005-four-game-changers` | Optimized; 4/0/0/0 | *MTG Commander Format* page, line 192 of `cmd.txt`. | Optimized; 4/0/0/0. Clean designed control row. |
| `population-001-bundled-heliod` | cEDH; 2/0/0/1 | `client/src/data/cedhDecks.ts`, `BundledCedh_HeliodBallista_Demo`. | Upgraded; 2/0/0/1. Owner-declared cEDH band miss; report-only. |
| `population-002-bundled-inalla` | cEDH; 9/0/0/6 | `client/src/data/cedhDecks.ts`, `BundledCedh_InallaThoracle_Demo`. | Optimized; 9/0/0/6. Owner-declared cEDH band miss; report-only. |
| `population-003-bundled-winota` | cEDH; 4/0/0/1 | `client/src/data/cedhDecks.ts`, `BundledCedh_WinotaKikiFelidar_Demo`. | Optimized; 4/0/0/1. Owner-declared cEDH band miss; report-only. |

## Regeneration

Run the pipeline in this order from the repository root. The skip flag keeps the checked-out MTGJSON vintage fixed and prevents a network refresh.

```bash
MTGJSON_SKIP_REFRESH=1 ./scripts/gen-card-data.sh
python3 scripts/gen-test-fixture.py
python3 scripts/gen-test-fixture.py --check
BRACKET_CORPUS_PRINT_EXPECTATIONS=1 \
  cargo test -p phase-engine --test integration \
  bracket_corpus::golden_expectations_match_the_engine -- --nocapture
```

Copy the printed canonical `ROWS_JSON` into `expectations.json`, then append a ratchet entry whose `engine_version` is strictly higher than the preceding entry, whose date and note explain the change, and whose `rows_digest` equals the printed digest. Never edit a label or citation merely to make it agree with the engine. The initial ratchet explicitly records that P0's frozen baseline predates the `FloorRule` table, so none of its rows were imported.
