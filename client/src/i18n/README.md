# i18n — internationalization

Two distinct concerns live under the `language` preference:

- **Chrome i18n (this directory):** app text — buttons, menus, settings, labels,
  game-log type labels — translated via **react-i18next**.
- **Content i18n (engine/card-data pipeline):** card names, rules text, type lines,
  and full-card images, sourced from MTGJSON `foreignData` and overlaid at the
  display layer. Not handled here — see `services/engineRuntime.ts` and
  `hooks/useEngineCardData.ts`.

## The boundary rule (read before adding any `t()`)

> **A string gets `t()` if and only if the frontend authored it.**
> Engine/card-database pass-through is left raw.

Never wrap with `t()`:

- Card Oracle text, token rules text, ability/reminder text.
- Card names, player names, object IDs.
- Engine enum strings interpolated into text (phase, mana type, counter type).
- `className`, CSS, data attributes, dev/debug strings, console logs.

In hybrid spots like `GameLog.formatEvent`, translate the **template** and
interpolate engine data raw:

```ts
// type label is chrome → translated; counterType/objectId are engine → raw
t("log.counterAdded", { counterType, count, objectId });
```

Content localization (translating the card text itself) is a *separate* mechanism
that sources official MTGJSON data — it does not happen via `t()`.

## Structure

```
i18n/
  index.ts            init (single instance; useSuspense: false; store-seeded lng)
  resources.ts        eager glob of all catalogs + SUPPORTED_LNGS + detectInitialLanguage
  react-i18next.d.ts  typed keys (English = type oracle)
  locales/<lng>/<ns>.json
```

Namespaces: `common` (default), `menu`, `game`, `deck-builder`, `draft`,
`settings`, `multiplayer`. **A component's namespace is its source directory**
(`components/draft/*` → `draft`), not its subject matter.

## Conventions

- Keys: nested dot paths, `camelCase` leaves, `<componentOrFeature>.<element>`.
- Opt into a namespace: `const { t } = useTranslation("game")`. `common` is implicit.
- Plurals: `key_one` / `key_other` + `t(key, { count })` (CLDR rules per locale).
  Don't hand-roll `count === 1 ? "" : "s"`.
- Numbers needing locale grouping: `{{value, number}}`.
- The store owns the active language; the i18n bootstrap mirrors it to i18next
  and `<html lang>`, so use `usePreferencesStore.getState().setLanguage(lng)`.
- English (`en`) is the typing oracle: add a key to `en/<ns>.json` **before**
  referencing it, or it won't type-check. Other locales fall back to English.
- Encoding: catalogs are **UTF-8 with literal accented characters** (write
  `"Wähle"` directly — never `\u`-escape sequences) so translations stay
  human-readable in diffs. Save files as UTF-8 with no BOM.
- Every other locale must carry the **exact same keys** as `en` — no missing
  translations, no orphans. `resources.test.ts` enforces both key parity and
  UTF-8 encoding across all catalogs (runs in CI + Tilt `test-frontend`).


## Japanese (`ja`)

Use concise, natural Japanese for frontend-authored text, with MTG terminology:
cast = 唱える, activate = 起動, resolve = 解決, priority = 優先権,
battlefield = 戦場, graveyard = 墓地, library = ライブラリー,
commander = 統率者 (the format is 統率者戦). Keep countering a spell (打ち消す)
distinct from counters (カウンター), and owner (オーナー) from controller
(コントローラー). Translate a complete sentence rather than joining English-order
fragments. Preserve optional choices, quantities, costs, and who may act.

Keep every interpolation token, formatter, markup tag, and shortcut intact.
Japanese uses the `other` plural category; keep all catalog keys for parity,
and make the `other` wording correct even for a count of one. Language selection
and persisted `ja-JP` normalization use the existing preferences store.

This catalog localizes UI text, not card identity, Oracle text, engine-authored
messages, or rules execution. Japanese card content uses the existing MTGJSON/Scryfall display-data pipeline:
`oracle-gen --sidecar-dir client/public` emits `card-data.ja.json`, and
`scripts/gen-scryfall-locale-images.sh` emits `scryfall-images.v2.ja.json`.
Both files are in `data-files.json` for publication alongside other locales.
Regenerate and publish these assets when enabling Japanese; UI catalogs alone
do not provide card content. Names and printed text currently cover single-faced
cards, following the existing exporter restriction. Art keeps the chosen printing
and uses its Japanese sibling when available, including face-specific image URLs.
Missing localized fields or printings fall back to English. Printed text can
differ from current Oracle wording; rules and card identity remain English.
Do not invent card translations in UI catalogs.

## Simplified Chinese (`zh`)

Use concise, natural mainland Simplified Chinese for frontend-authored text,
with official Wizards 万智牌 terminology: cast = 施放, activate = 起动,
resolve = 结算, priority = 优先权, battlefield = 战场, graveyard = 墓地,
library = 牌库, hand = 手牌, commander = 指挥官 (the format is 指挥官),
planeswalker = 鹏洛客, token = 衍生物. Keep countering a spell (反击) distinct
from counters (指示物), controller (操控者) from owner (拥有者), and 牌库
(library) from 套牌 (deck). Translate a complete sentence rather than joining
English-order fragments. Preserve optional choices, quantities, costs, who may
act, and every interpolation token, formatter, markup tag, and shortcut.

Chinese resolves only the `other` CLDR plural category, so `_one` / `_few` /
`_many` values are unreachable at runtime — they still must exist for key parity,
but **the `_other` wording must read correctly at every count, including one**.
This differs from Japanese only in emphasis, not in rule.

### Chinese is the one locale whose card content comes from elsewhere

Every other locale's card text is MTGJSON `foreignData`, emitted by
`oracle-gen --sidecar-dir`. Chinese cannot be: **MTGJSON carries no Simplified
Chinese for any set released after 2024-07-05 (`ACR`)** — Bloomburrow, Duskmourn,
Foundations, Aetherdrift and every later set are 0% — so the MTGJSON pipeline
would ship a catalog with no data for any current set.

Instead, `scripts/gen-zhs-card-data.sh` compiles `card-data.zh.json` from the
[magic-cards-zhs](https://github.com/HeliumOctahelide/magic-cards-zhs) weekly
data export, and `scripts/lib/magic_cards_zhs.py` holds the ingest. Read that
file's docstring before touching the pipeline: the upstream JSONL is **not
strictly valid JSON** (escaped quotes are double-escaped, `\\"` for `\"`, which
breaks ~13% of lines under a strict parser) and its newlines are double-escaped
as well. Both are repaired at the byte level, and the docstring records the
measured evidence for each.

Its records are oracle-level and per-face, so unlike the MTGJSON path Chinese
covers **multi-face cards** too. The sidecar is still keyed by lowercased English
face name, matching `ensureCardLocale` and the engine's `face_index`; records
whose English name contains `//` are skipped, because that key can never be a
face name and splitting it would let a back face clobber the standalone card
sharing its name.

Chinese card ART comes from [mtgch.com](https://mtgch.com/) instead of the
Scryfall locale maps. `services/cardArtSource.ts` derives the URL from the
printing id the client already holds, so no sidecar map is needed — and Scryfall
serves no Chinese printings under those ids, which is why
`gen-scryfall-locale-images.sh` has no Chinese entry. Two consequences worth
knowing: mtgch publishes **no `art_crop`** for Chinese (that endpoint 404s), so
crop requests fall back to the English crop; and the host resolves to
Chinese-network IPs that can refuse connections from elsewhere, which is why the
art source is a user-facing setting and every failure degrades to English art.

Both sources are opt-out-able in Settings → Gameplay (`Card Text`, `Card Art`)
and neither is redistributed as art: images are fetched from the user's browser
at runtime. Card text IS redistributed, in `card-data.zh.json` — a derivative
work of a CC BY-SA 4.0 data set. See [NOTICE](../../../NOTICE) and
[DMCA.md](../../../DMCA.md) before changing how it is shipped.

Card identity, the engine's Oracle parse, and rules execution remain English in
every locale. `chineseCardText` only gates Chinese; it must never become a
general on/off switch for content i18n, because for every other locale the
sidecar is the sole source of card text.

