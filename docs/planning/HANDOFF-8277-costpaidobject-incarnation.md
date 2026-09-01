# Handoff: `CostPaidObject` incarnation validation (upstream issue #8277 + PR #8265 HIGH finding)

**You are picking up deferred engine work.** Everything you need is below. Read it fully before editing — the scope has already been measured, and prior attempts at adjacent fixes were rejected for reasons documented here.

---

## 1. The bug, in one paragraph

`TargetFilter::CostPaidObject` is an untargeted back-reference (CR 608.2k) to the object named by an ability's cost or trigger condition. Its referent is stored as `CostPaidObjectSnapshot { object_id: ObjectId, lki: LKISnapshot }` (`crates/engine/src/types/ability.rs:8497-8500`). **That struct records no incarnation.** Under CR 400.7 an object that changes zones becomes a *new object*, but the engine reuses `ObjectId` as stable storage identity. So if the referent leaves and returns before the follow-up effect resolves, every consumer that resolves `CostPaidObject` to a bare `ObjectId` will act on the **new object** as though it were the original.

## 2. Proof it is real (measured, not theorised)

A probe against the live resolver at `0fada6228` printed:

```
PROBE incarnations: start=0 afterGY=1 afterReturn=2 backOnBf=true
PROBE target_pin_is_current(referent) = true    <-- VACUOUS
PROBE target_incarnations len = 0
PROBE referent_still_bf=false bystander_still_bf=true spare_still_bf=true
```

The referent left and returned under the same `ObjectId` with incarnation bumped `0 -> 1 -> 2`, the guard returned `true` anyway, and **the new incarnation was sacrificed**.

Why the guard is vacuous: `ResolvedAbility::target_pin_is_current` (`types/ability.rs:~28739`) is documented **"FAIL-OPEN AT THE LOOKUP"** — it does `...find(|pin| pin.object_id == id).is_none_or(|pin| pin.is_current(state))`, so with **no pin recorded it returns `true`**. A `CostPaidObject` referent never has an ordinary target pin, so the check passes for free. Do not reuse that function for this purpose.

## 3. Critical constraint discovered while scoping — read this first

**`LKISnapshot` does NOT carry an incarnation field.** Verified: `awk '/pub struct LKISnapshot/,/^}/' crates/engine/src/types/game_state.rs | grep -c incarnation` returns **0**. It carries `name`, `power`, `toughness`, `base_power`, `base_toughness`, `mana_value`, `controller`, `owner`, core types — characteristics, not identity epochs.

**Consequence:** the captured incarnation is *not recoverable* from what `CostPaidObjectSnapshot` stores today. There is no clever read that avoids changing the type. **The fix necessarily means capturing the incarnation at binding time**, which is exactly why this was split out of PR #8265 rather than patched narrowly.

### 3a. The near-miss — a third source exists and is DISQUALIFIED. Do not rediscover it.

`state.zone_changes_this_turn` **does** carry `entered_incarnation` (`types/game_state.rs:1580`), and a probe confirmed it records a round trip:

```
rec[1] obj=ObjectId(1) from=Some(Graveyard) to=Battlefield entered_inc=Some(2)
```

That looks like it would allow a narrow in-file fix with no type change. **It does not, for two measured reasons:**

1. **`game/turns.rs:1374` clears the ledger at cleanup.** A guard built on it is correct within a turn and **silently fail-open across a turn boundary** — precisely for delayed and stack-timed sacrifices, which are the shape most likely to cross one.
2. **A directly-created battlefield object has no ledger entry at all** (`PROBE-C ledger_len=0`), making it indistinguishable from a never-departed object.

Shipping that with a comment claiming incarnation validation would be a third overclaiming guard in `sacrifice.rs` (two were already rejected in review). It is recorded here so nobody spends a cycle rediscovering it and assuming it was missed.

## 4. The right primitive already exists — reuse it

`ObjectIncarnationRef` (`crates/engine/src/types/identifiers.rs:147`):

```rust
pub struct ObjectIncarnationRef { pub object_id: ObjectId, pub incarnation: u64 }
impl ObjectIncarnationRef {
    pub fn of(object_id: ObjectId, incarnation: u64) -> Self;
    pub fn from_object(obj: &GameObject) -> Self;
    pub fn is_current(&self, state: &GameState) -> bool;  // strict full-pair compare
}
```

It already has `LEGACY_INCARNATION: u64 = u64::MAX` (`identifiers.rs:140`) and an **untagged serde compat shim** (`ObjectIncarnationRefCompat`) accepting both the new `{object_id, incarnation}` map and a legacy bare-number `ObjectId`. **That shim is your model for the save-compat problem below.** Do not invent a parallel mechanism.

## 5. Measured scope — do not re-derive, but do re-verify before relying on it

| Metric | Value | How measured |
|---|---|---|
| `CostPaidObjectSnapshot {` construction sites | **58** | `grep -rn "CostPaidObjectSnapshot {" crates/engine/src --include=*.rs \| wc -l` |
| Files containing those sites | **21** | same, `-l \| wc -l` |
| `cost_paid_object` references | **~240** | `grep -rn "cost_paid_object\b" crates/engine/src --include=*.rs \| wc -l` |

Files: `game/{ability_utils,casting,casting_costs,casting_tests,engine_combat,engine_resolution_choices,filter,mana_abilities,quantity,stack,triggers,visibility}.rs`, `game/effects/{amass,copy_spell,dig,effect,manifest,mod,sacrifice,token_copy}.rs`, `types/ability.rs`.

**Many of the 58 are inside `#[cfg(test)]`. Count production vs test separately** — it materially changes the risk picture and the previous scoping did not split them.

### Known consumers to fix (verify and extend — this list is not proven complete)
- `game/targeting.rs:~794-809` — `resolved_targets` `CostPaidObject` arm. **The documented canonical chokepoint**; its own comment calls it "the general chokepoint for every effect that targets a cost-paid object."
- `game/filter.rs:~3424-3443` — identity matching
- `game/quantity.rs` — `ObjectScope::CostPaidObject` P/T ladder
- `game/effects/sacrifice.rs` — see section 7; already partially handled

## 6. THE OPEN DESIGN DECISION — deliberately left for you

`CostPaidObjectSnapshot` derives `Serialize`/`Deserialize`. Adding a required field is a **save-format change**. A legacy save has no recorded incarnation, so you must decide what that means:

**Fail-open** (e.g. `#[serde(default)]` to `LEGACY_INCARNATION`, treated as current)
- Pro: never breaks an in-flight saved game; no user-visible regression on load
- Con: preserves today's bug for those saves — the exact defect you are fixing stays live for them

**Fail-closed** (absent incarnation treated as stale)
- Pro: rules-correct everywhere immediately; no silent wrong-object actions
- Con: silently no-ops these effects in existing saved games, which players experience as "my card did nothing"

**A third option worth weighing:** an untagged compat shim mirroring `ObjectIncarnationRefCompat`, so legacy saves deserialize into an explicitly-legacy variant each consumer handles deliberately rather than through one global default.

This was left open **on purpose** — it is a genuine product judgement with user-visible consequences, not a mechanical choice. Pick one, justify it in the PR body, and expect a maintainer to have an opinion. Also decide per-consumer what "stale" *does*: sacrifice no-ops cleanly, but a stale referent in the `quantity.rs` P/T ladder silently reading `0` is not obviously correct.

## 7. What already shipped in PR #8265 (do not redo, do not revert)

PR #8265 (Victimize, upstream issue #7898) contains a **narrow** `CostPaidObject` fix in `game/effects/sacrifice.rs` only: it resolves the referent through `targeting::resolved_targets`, requires it to still be a battlefield permanent (CR 701.21a), and makes an absent/departed referent a hard no-op that never falls through to the untargeted pool. Five inline tests cover both ladder rungs present/departed plus unbound.

**That fix is correct as far as it goes but cannot validate incarnation**, for the section 3 reason. When your change lands, revisit that code and tighten it to use the real incarnation check — and delete any comment there that overclaims. **Two prior comments in that file were rejected in review for overclaiming**; do not add a third. State exactly what is and is not guaranteed.

## 8. Also deferred, related but distinct — upstream issue #8277

Separate hazard, same filter: effects that call `effect_object_targets` **bypass** the canonical chokepoint and inherit parent targets. `effects/mod.rs:261-267` returns every inherited object for any filter but `SpecificObject`/`ParentTargetSlot`, and `can_inherit_parent_targets` (`effects/mod.rs:3100-3117`) excludes only `references_exiled_by_source` — **not** `CostPaidObject`.

Sharpest case: `effects/perpetual.rs:73` returns propagated inherited `ability.targets` *before* reaching `resolved_targets` at `:92`. Six sites pass raw `ability.targets` (`perpetual.rs:73`, `stickers.rs:43`, `stickers.rs:135`, `cloak.rs:147`, `manifest.rs:118`, `exile_face_down_pile.rs:82`); five pass `live_object_targets` (`gain_control.rs:225`, `gain_control.rs:356`, `attach.rs:424`, `remove_from_combat.rs:42`, `effect.rs:820`). Verified **not** exposed (they route through `resolved_targets` first): `change_zone.rs:678`, `conjure.rs:284`, `double.rs:280`, `pump.rs:42`, `pump.rs:235`, `put_on_top.rs:116`, `reveal.rs:41`, `switch_pt.rs:30`, `token_copy.rs:182`, `mod.rs:282`.

The likely class fix is excluding `CostPaidObject` in `can_inherit_parent_targets` alongside `references_exiled_by_source` — but that changes propagation for **every** inheriting effect and has two further consumers in `search_library.rs:566,579`. **Decide whether to fold #8277 into your PR or keep it separate; they are genuinely coupled** (both are "CostPaidObject is not a declared target"), and doing both at once may be cleaner than sequencing them.

**Honest limit on #8277, stated in the issue:** the exposure is **structurally proven but not corpus-quantified**. Nobody enumerated which real cards pair `CostPaidObject` with those effects — `card-data.json` is gitignored and absent from the checkout. Some sites may be latent. If you need the corpus, run `./scripts/gen-card-data.sh`.

## 9. Required regression test (a maintainer specified this)

> "Add a regression that moves the referent away and back before resolution while an inherited live parent target/bystander is present, proving neither new incarnation nor bystander is sacrificed."

Concretely: bind the referent, `crate::game::zones::move_to_zone` it out and back (this bumps incarnation — confirmed `0 -> 1 -> 2` above; two bumps, one per zone change), keep a live inherited parent target and an unrelated bystander on the battlefield, resolve, and assert **neither** the returned new incarnation **nor** the bystander is affected. Add the paired positive (referent never departed, so it *is* acted on).

Helpers already exist in `sacrifice.rs`: `make_battlefield_creature`, `make_cost_paid_sacrifice` (~:2080), and five example tests at ~:2122-2320. **Prove every new test revert-failing by mutation and paste the failure output in the PR** — this project rejects tests that pass in both directions, and that has already happened twice on #8265.

## 10. Environment (Windows, verified working)

- Toolchain pinned by `rust-toolchain.toml` to **nightly-2026-04-19**; rustup auto-selects inside the repo.
- **Tilt is installed but NOT running.** `tilt get uiresource clippy` exits 1. Per CLAUDE.md that means direct cargo is the correct fallback. **Never report anything as Tilt-verified.** A `tilt-wait` exit `3` means "could not determine", not "failed".
- **Python**: real interpreter at `C:\Users\jacob\AppData\Local\Programs\Python\Python313`. **Prepend it to PATH** or the Windows Store `python3` stub shadows it and `cr733_authority_matrix_covers_the_fresh_write_census` falsely fails.
- **pnpm is NOT installed.** Declare frontend gates explicitly N/A rather than silently omitting them.
- Use an isolated `CARGO_TARGET_DIR` (e.g. `D:\wt\tgt-8277`). **Never `cargo clean`.** Full suite ~26,347 tests; the integration binary alone takes ~19 min. Cold builds ~10-20 min.
- **Git worktrees must use a SHORT path** (e.g. `D:/wt/<name>`). Deep paths blow Windows `MAX_PATH` on this repo's long snapshot filenames — `core.longpaths` is unset and the OS flag needs elevation.
- `docs/MagicCompRules.txt` is **gitignored**; run `./scripts/fetch-comp-rules.sh` once. Every CR number must be grep-verified before it goes in code, and the rule *body* must describe the annotated code.

### Parser-probe traps (these produced a false blocker on #8265)
If you probe the parser: `parse_oracle_text` **overflows the default 8MB stack** (`STATUS_STACK_OVERFLOW`, `0xc00000fd`) — spawn a 256MB thread. It also returns **bogus `Unimplemented`** in a bare `examples/` harness even in release; use `parse_effect_chain`. The lib crate is **`engine`**, not `phase_engine`. Serialize the whole `AbilityDefinition`, not just `.effect`. **Always run a positive control with a known-passing committed string first — if the control fails, your instrument is broken, that is not a finding.**

## 11. Process lessons from #8265 — these cost real review cycles

1. **A zero-hit measurement proves nothing without path coverage.** An agent instrumented a guard, measured zero hits across 26,242 tests, and concluded dead code. Wrong: 9 of 10 tests staged a fixture that took the *other* branch. Produce a **path-coverage table** (which test drives which branch, measured) before inferring absence.
2. **"Disable X, 0 tests fail" is a statement about the tests, not the code.**
3. **Do not write comments that overclaim.** Two comments in `sacrifice.rs` were rejected for asserting safety that did not hold. If a guard is partial, say so in the comment.
4. **Every negative assertion needs a paired positive reach-guard** on the same fixture, or it is vacuous.
5. Verify a reviewer's premises against source before implementing — but note that on #8265 the maintainer was right every single time.

## 12. Definition of done

- [ ] Incarnation captured at the binding seam(s) and validated by every `CostPaidObject` consumer
- [ ] Stale incarnation is a **hard no-op** per consumer, with per-consumer semantics justified
- [ ] Save-compat decision made **and defended in the PR body** (section 6)
- [ ] The section 9 regression plus its paired positive, both proven revert-failing with pasted output
- [ ] Production vs test construction sites counted separately
- [ ] Decision recorded on whether #8277 folds in (section 8)
- [ ] `cargo fmt --all`, `cargo clippy -p phase-engine --all-targets -- -D warnings`, full `cargo test -p phase-engine` — all green, unrounded counts reported
- [ ] Every CR citation grep-verified with line numbers; rule bodies actually describe the code
- [ ] PR #8265's narrow `sacrifice.rs` fix revisited and tightened once this lands

## 13. Pointers

- Upstream repo `phase-rs/phase`; fork `JacobWoodson/phase`. **Issues live upstream, never on the fork.**
- This branch: `fix/8277-costpaidobject-incarnation`, cut from **`upstream/main`** (deliberately not from the #7898 work, so it can land independently).
- Related: PR **#8265** (Victimize, has the narrow fix plus full review history), issue **#8277** (the `can_inherit_parent_targets` class hazard), issue **#7898** (the original Victimize bug report).
- The maintainer reviewing this area is **@matthewevans**. He reviews precisely, cites `file:line`, and has been correct on every finding so far. Expect the same standard.
