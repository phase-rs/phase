# Third-party Chinese card sources — provenance, licence, and integrity

This document exists because the Chinese card-content sources are the only
external data this project takes on beyond MTGJSON and Scryfall, and because
adopting them is a maintainer decision rather than a purely technical one. It
records what each source is, why nothing else can do the job, what its licence
actually says (including where that licence is silent), and how a downloaded
artifact is proven to be the artifact that was reviewed.

## Why a third-party source is needed at all

**Wizards of the Coast stopped printing Simplified Chinese physical cards, and
the official Chinese card corpus therefore ends in 2024.** From the official
Chinese-language announcement, *万智牌实体卡牌产品语言调整通知*, published
2024-02-23 ([cnwizards.com/article/42765](https://www.cnwizards.com/article/42765)):

> 自《Bloomburrow》开始停止生产中文版实体卡牌产品。
> 《摩登新篇3》发布后，万智牌实体卡牌产品的语言将包括英语、日语、法语、意大利语、德语和西班牙语。

("Production of Chinese-language physical card products stops from *Bloomburrow*
onward. After *Modern Horizons 3*, Magic physical card products will be
available in English, Japanese, French, Italian, German and Spanish.")

That is measurable in the data this project already consumes. MTGJSON's
`foreignData` carries **no Simplified Chinese for any set released after
2024-07-05 (`ACR`)** — measured over the full 875-set corpus (114,694 records),
every set from *Bloomburrow* (`BLB`, 2024-08-02) onward is 0%. Of the 41,458
records that do carry a Chinese entry, 4,477 are English placeholder names
rather than translations.

So the existing MTGJSON pipeline cannot serve Chinese: reusing it would ship a
catalog that is empty for **every set released in the last two years**, which is
exactly the sets a new player opens. The two community projects below are what
remains: they maintain Simplified Chinese for the sets Wizards no longer prints
in Chinese.

**The Chinese text is display-only.** Card identity, the engine's Oracle parse,
rules execution, deck files, and every lookup key stay English. The overlay is
keyed by lowercased English face name, which is also the engine's `face_index`
key, and it falls back per field to English. Nothing in the engine reads Chinese
text, and no card can behave differently because Chinese is selected.

## Source 1 — magic-cards-zhs (card text)

| | |
|---|---|
| Project | [HeliumOctahelide/magic-cards-zhs](https://github.com/HeliumOctahelide/magic-cards-zhs) |
| What it is | Simplified Chinese translations of all MTG cards, published as a weekly data export |
| Licence | **CC BY-SA 4.0** (`LICENSE-CC-BY-SA.txt`, unmodified CC boilerplate) |
| Cadence | Weekly releases; 44 tags since 2025-04-08, most recent `data-2026-10-11` |
| Ingests | `zhs_oracle.json` only, via `scripts/lib/magic_cards_zhs.py` |
| Ships as | `card-data.zh.json` — a derivative work, redistributed under the same CC BY-SA 4.0 |

**Why this one and not MTGJSON.** Its export is *oracle-level and per-face*,
carrying the English name alongside each translation, so it needs no join and
covers multi-face cards — which the MTGJSON path cannot, because
`foreignData` is card-level with one combined `"A // B"` name and no reliable
per-face split.

**Licence caveats, stated rather than glossed.** The repository's licence is
stock CC BY-SA 4.0 with no copyright notice, no attribution notice, and **no
mention of Wizards of the Coast anywhere**. The README has no licence section at
all; it says only that English text comes from Scryfall and the Chinese from
official text, MTGso, and volunteer translations. The corpus is substantially
Wizards card text, so the upstream grants a licence it is not in a position to
grant cleanly. **This project inherits that gap rather than resolving it**, which
is why `NOTICE` and `DMCA.md` say so explicitly and why the sidecar is generated
by a self-contained script and is one manifest entry to remove.

## Source 2 — mtgch.com / 大学院废墟 (card art)

| | |
|---|---|
| Site | [mtgch.com](https://mtgch.com) (also served as `sbwsz.com`) |
| What it is | Chinese MTG reference site; mirrors the Simplified Chinese printing of each English Scryfall printing **under the same Scryfall printing id** |
| Licence | **No published licence.** Its *开放使用说明* states the database and card images are open to all developers |
| How it is used | Images are fetched from its CDN **by the user's browser at runtime**. Nothing is redistributed |
| Opt-in | Off unless the player selects Chinese art; the switch is in Settings → Gameplay |

The *开放使用说明* ("open use statement", from the site's about article) says:

> 大学院废墟的数据库和卡牌图像向所有人公开，供有需要的开发者使用。

("The 大学院废墟 database and card images are open to everyone, for developers
who need them.") **That is a statement of intent, not a licence instrument** — it
names no terms, no version, and no revocation policy, and the site's own footer
places it under Wizards' Fan Content Policy. We therefore rely on it only for the
narrow thing it actually grants: fetching images in a client, over the network,
at the user's request. This project redistributes none of it.

**Also recorded, because it is relevant to any automated fetch:** the site's
`robots.txt` disallows `ClaudeBot`, `GPTBot`, and `Amazonbot` outright. The
wildcard group permits the API and the image CDN paths. The client's use is a
browser fetching a user-requested image, which is the ordinary user-agent case,
but no crawler is pointed at this site by this project, and the build pipeline
never contacts it — only `images.mtgch.com` is contacted, and only from the
user's browser.

## Integrity — how a download is proven to be the reviewed artifact

The pipeline downloads one archive from GitHub Releases. Every control below is
deterministic and checkable without trusting the transport:

1. **Content pinning.** `scripts/magic-cards-zhs.lock` records the release tag
   and the **SHA-256 of the archive**, and the generator verifies every download
   against it before doing anything with the bytes. A mismatched or truncated
   download is deleted and the run fails.
2. **The digest is independently corroborated, not self-asserted.** GitHub
   publishes a per-asset `digest` on its Releases API. For the pinned release
   `data-2026-10-11`:

   ```
   GitHub Releases API : sha256:ccb0badbd43a583d54eff740f8f828e50e8a92737c992f13cd630ae801ca590d
   lock file / computed: sha256:ccb0badbd43a583d54eff740f8f828e50e8a92737c992f13cd630ae801ca590d
   ```

   They agree, so the pinned value is not merely "whatever we downloaded once".
   Anyone can reproduce that comparison with
   `curl -s https://api.github.com/repos/HeliumOctahelide/magic-cards-zhs/releases | jq -r '.[0].assets[0].digest'`.
3. **Archive integrity before use.** `gzip -t` runs on the archive; a failure
   deletes it rather than leaving a partial file that a later `-C -` resume would
   append to.
4. **No stale data can be silently reused.** The extract is re-derived into a
   scratch directory on every run, so bumping the pinned tag cannot rebuild from
   an older member left on disk or restored from a CI cache.
5. **The cache stores only the pinned artifact.** The deploy cache key is derived
   from the generator's hash (which pins the tag), and the save is gated on the
   archive actually existing — a truncated download cannot be frozen under a key
   later runs restore.
6. **Output determinism.** The builder's winner for a duplicated English name is
   chosen by record properties — most complete rules text, then longer text, then
   longer name — never by iteration order, so two runs over the same export
   produce byte-identical output. Verified by running it twice.

**What is deliberately not attempted.** The Chinese *text* is community-authored
and cannot be cryptographically vouched for; pinning proves *which artifact* is
in use and that it is intact, not that its contents are correct. That is the same
posture this project already takes toward MTGJSON and Scryfall — the difference
is that those two are the upstream authorities for their data, and this one is a
community mirror of a corpus whose publisher has stopped producing it.

## Reproducing all of the above

```bash
# What the release actually is, and its published digest
curl -s https://api.github.com/repos/HeliumOctahelide/magic-cards-zhs/releases?per_page=1 \
  | jq -r '.[0] | .tag_name, .assets[0].digest'

# What we pin, and that a fresh download matches it
cat scripts/magic-cards-zhs.lock
./scripts/gen-zhs-card-data.sh          # verifies the pin, then builds

# That the output is deterministic
python3 scripts/lib/magic_cards_zhs.py \
  --source data/magic-cards-zhs/zhs_oracle.json --output /tmp/a.json
cmp client/public/card-data.zh.json /tmp/a.json && echo byte-identical
```
