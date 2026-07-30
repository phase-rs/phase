# MSH Wave 6 — Labeled-card findings (verified against current code, 2026-06-20)

Coverage snapshot client/public/coverage-data.json is STALE (connive trigger cards
already dropped off the unsupported list after concurrent connive-infra merge).
Re-verify every card against current parser before implementing.

## Labeled gap cards (gap_count>=1) — all SINGLETONS, distinct primitives

| Card | Gap | Current state | Fix |
|------|-----|---------------|-----|
| Loki, God of Mischief | "a player or permanent becomes the target of an ability you control … only once each turn" | `TriggerMode::BecomesTargetOnce` EXISTS (triggers.rs:297); parser handles "attacks/blocks/becomes the target of a spell" (oracle_trigger.rs:4857) but not this subject ("player or permanent") + restriction ("an ability you control") | Parser extension only — map subject+"ability you control"→BecomesTargetOnce. SMALL. |
| Puppet Master, String Puller | "Whenever one or more goaded creatures deal combat damage to one of your opponents" | NO goad-combat-damage trigger in oracle_trigger.rs (only goad EFFECT + on-attack goad). First ability (goad on attack) likely parses. | NEW trigger condition: goaded-creatures-deal-combat-damage. MEDIUM. |
| Leader, Super-Genius | "If a creature you control would connive, instead you draw a card, then that creature connives." | connive TRIGGER now parses (concurrent); the REPLACEMENT does not. 2nd ability (combat→connive) likely OK now. | connive replacement (ReplacementMode). MEDIUM. /add-replacement-effect. |
| Call Damage Control | "Choose up to two. Return those cards from your graveyard to your hand. • Target artifact/creature/enchantment/land card." | gap label "target" x4 — modal-with-per-mode-targets + return-from-graveyard | Ties into Group III modal (choose up to N) + per-mode TargetFilter. Defer w/ Group III. |

## Other explicit-label cards (from coverage scan)
- The Mind Stone -> "harness" (test harness artifact, not a real gap — investigate separately)
- Marvel Boy, Noh-Varr -> KeywordAbilityActivated(PowerUp) — Power-up trigger (relates to shipped Power-up kw)
- Galactus / M.O.D.O.K. -> "unknown"

## Decision
None of the labeled cards clusters with mates. The 49 gap=0 misparses are where shared-primitive ROI lives — diagnostic agent re-clustering those against current code. Pick the highest-ROI gap=0 cluster as the next wave; handle these singletons opportunistically.
