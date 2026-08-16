# MSH Wave 6 — wave selection decision (2026-06-20)

## Full gap=0 triage clusters (diagnostic agent a57689144fdb999ce)
- Cluster 1: mana spend-restriction surface (Karolina Dean neg / Ronin disjunction) — S, parser/oracle_effect/mana.rs
- Cluster 2: defender-scoped can't-attack restriction (Willie Lumpkin / Promise of Loyalty) — M, statics.rs+combat.rs+imperative.rs
- Cluster 3: counter mirror/replicate (Captain Marvel "same number and kind" / Super-Adaptoid "do the same for") — M/L, ability.rs+counter.rs

## Open-PR collision audit (user-requested "ensure none covered by PRs")
- Card-name duplication: ZERO hits across 19 open PRs.
- Cluster 1 DISQUALIFIED: #4011 (Whovencroft, OPEN/CLEAN) is rewriting parse_mana_spend_restriction
  for activation-first disjunctions — exact function Ronin needs; Ronin's cast-first disjunction
  likely already routes through #4011's disjunction parser. DEFER until #4011 merges; then
  Karolina's negative-restriction half remains as follow-up.
- Cluster 2: no open-PR FUNCTION overlap. Shared files statics.rs (#4010/#3958 add DIFFERENT
  variants — additive, low risk), imperative.rs (#4012/#3958 edit discover/suspect funcs, not
  the can't-attack parser at :9204). combat.rs gate UNCONTESTED.
- Cluster 3: counter.rs UNCONTESTED; ability.rs additive variant.

## SELECTED: Cluster 2 (defender-scoped can't-attack)
Rationale: highest building-block value ("can't attack you/your permanents [next turn]" =
Propaganda/Peacekeeper/Blazing-Archon family), runtime mostly exists, no function-level PR conflict.
Key architectural Q for planner: parameterize StaticMode::CantAttack with a defender scope vs new
scoped variant — mind that combat.rs:440/444/451 match CantAttack directly (field-add = blast radius)
and statics.rs is concurrently edited by #4010/#3958 (additive). CR refs: 508.1c (attack
restrictions), 509.1b. Verify against docs/MagicCompRules.txt.

## Deferred singletons (uncontested, future waves): Flying Drone (colored-shard cost-red, S),
## Ultimate Nullification (2nd mass-exile subject, S/M), Lady Loki (difference-quantity, M),
## Winter Soldier Reborn (attack-trigger return-to-bf assembly, M).
