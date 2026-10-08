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
