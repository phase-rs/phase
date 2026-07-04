//! Group C — Throw from the Saddle and the "X instead if Z. Then Y." class
//! (S01 reflexive-if completion).
//!
//! Four coordinated parser fixes plus one resolver fix make a trailing,
//! independent clause printed AFTER a `ConditionInstead` override run in BOTH
//! branches (CR 608.2c — read the whole text; the "instead" replaces only the
//! prior sentence):
//!   - FIX A  (`parser/oracle.rs`): defer an intra-chain "… instead if <cond>.
//!     <trailing clause>" line to the chunk parser instead of swallowing it.
//!   - FIX A' (`parser/oracle_effect/lower.rs`): rebind the instead-override
//!     "Put a +1/+1 counter on it" `SelfRef` → `ParentTarget` (the chosen base
//!     target).
//!   - FIX C  (`parser/oracle_effect/lower.rs`): rebind the nested one-sided-fight
//!     damage tail to the Ambuscade anaphor (`Power{Anaphoric}` +
//!     `DamageSource::Target`).
//!   - RESOLVER (`game/effects/mod.rs`): in the `ConditionInstead` not-swap arm,
//!     run the override's trailing `SequentialSibling` tail with the swap path's
//!     one-sided-fight target contract, when `else_ability` is absent.
//!
//! Every behavioral test drives the real cast pipeline (`GameRunner::cast(...)
//! .resolve()`), so it exercises parser + resolver end-to-end and fails if the
//! corresponding fix is reverted.
//!
//! MEASURED RUNTIME STRUCTURE (the key subtlety): the resolver fix's
//! behavior-change set is exactly the four cards whose independent tail becomes a
//! NESTED `SequentialSibling` under the `ConditionInstead` override AT RUNTIME:
//! Throw from the Saddle (DealDamage tail), Evil's Thrall (Untap+haste tail),
//! Take the Fall and That's Rough Buddy (Draw tail). For the latter two, "Draw a
//! card." prints on a SEPARATE LINE, so the RAW `parse_oracle_text` yields it as
//! a separate top-level `ability[1]`; but `synthesize_all` (run by
//! `from_oracle_text` and the card-data pipeline) FOLDS that ability into
//! `ability[0]`'s deepest sub — the override — as a `Draw[SequentialSibling]`
//! tail. So at runtime all four carry the nested tail the resolver fix runs.
//! Measured + revert-probed: disabling the resolver `else if` branch drops the
//! Draw on Take the Fall / That's Rough Buddy as well as the damage/untap on
//! Throw / Evil's Thrall. (FIX A is still required for the latter two: without it
//! the line-level swallow loses the Draw clause before synthesis can fold it.)

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, DamageSource, Effect, ObjectScope, QuantityRef};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THROW: &str = "Target creature you control gets +1/+1 until end of turn. \
Put a +1/+1 counter on it instead if it's a Mount. \
Then it deals damage equal to its power to target creature you don't control.";

const AMBUSCADE: &str = "Target creature you control gets +1/+1 until end of turn. \
It deals damage equal to its power to target creature an opponent controls.";

const EVIL_THRALL: &str = "Gain control of target creature until end of turn. \
If you control a Villain with greater mana value than that creature, gain control \
of that creature until the end of your next turn instead. Untap that creature. \
It gains haste until end of turn.";

const TAKE_THE_FALL: &str = "Target creature gets -1/-0 until end of turn. \
It gets -4/-0 until end of turn instead if you control an outlaw. \
(Assassins, Mercenaries, Pirates, Rogues, and Warlocks are outlaws.)\n\
Draw a card.";

const THATS_ROUGH_BUDDY: &str = "Put a +1/+1 counter on target creature. \
Put two +1/+1 counters on that creature instead if a creature left the battlefield \
under your control this turn.\n\
Draw a card.";

fn ct() -> Vec<String> {
    vec!["Creature".to_string()]
}

/// Depth-first search for the first `DealDamage` effect in an ability chain.
fn find_deal_damage(def: &AbilityDefinition) -> Option<&Effect> {
    if matches!(&*def.effect, Effect::DealDamage { .. }) {
        return Some(&def.effect);
    }
    def.sub_ability
        .as_deref()
        .and_then(find_deal_damage)
        .or_else(|| def.else_ability.as_deref().and_then(find_deal_damage))
}

// ===========================================================================
// PARSER (production `parse_oracle_text`) — FIX A + FIX A' + FIX C structural
// gate. Each assertion flips when the corresponding fix is reverted.
// ===========================================================================

/// FIX A reroutes Throw to the chunk parser (else the whole damage clause + the
/// Mount gate are swallowed at the line level). FIX A' binds the override
/// counter to `ParentTarget`. FIX C rebinds the nested damage tail to the
/// Ambuscade anaphor. The assembled `DealDamage` node must be byte-identical to
/// Ambuscade's: `amount = Ref{Power{Anaphoric}}`, `damage_source = Target`.
#[test]
fn throw_parses_to_full_chain_with_ambuscade_damage_shape() {
    let throw = parse_throw();
    let ambuscade =
        engine::parser::oracle::parse_oracle_text(AMBUSCADE, "Ambuscade", &[], &ct(), &[])
            .abilities
            .remove(0);

    // FIX A' — the override counter binds to the chosen base target.
    let put_counter = throw
        .sub_ability
        .as_deref()
        .expect("Throw root Pump must carry the instead-override sub-ability");
    match &*put_counter.effect {
        Effect::PutCounter { target, .. } => assert_eq!(
            format!("{target:?}"),
            "ParentTarget",
            "FIX A': the override counter must bind to ParentTarget (the chosen base \
             creature), not SelfRef (the spell). Reverting FIX A' leaves SelfRef."
        ),
        other => panic!("override effect must be PutCounter, got {other:?}"),
    }

    // FIX C — the nested damage tail matches the Ambuscade one-sided-fight shape.
    let throw_dd = find_deal_damage(&throw).expect("Throw must produce a DealDamage tail");
    let ambuscade_dd =
        find_deal_damage(&ambuscade).expect("Ambuscade must produce a DealDamage node");
    assert_eq!(
        throw_dd, ambuscade_dd,
        "FIX C: Throw's nested damage tail must be byte-identical to Ambuscade's \
         one-sided-fight node. A mismatch means it kept the broken Power{{Source}} + \
         damage_source:None default (0 damage from the spell)."
    );
    match throw_dd {
        Effect::DealDamage {
            amount,
            damage_source,
            ..
        } => {
            assert_eq!(
                *damage_source,
                Some(DamageSource::Target),
                "FIX C: damage source must be Target (the boosted creature)"
            );
            assert!(
                matches!(
                    amount,
                    engine::types::ability::QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Anaphoric
                        }
                    }
                ),
                "FIX C: amount must be Power{{Anaphoric}}, got {amount:?}"
            );
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

fn parse_throw() -> AbilityDefinition {
    let mut parsed =
        engine::parser::oracle::parse_oracle_text(THROW, "Throw from the Saddle", &[], &ct(), &[]);
    assert_eq!(
        parsed.abilities.len(),
        1,
        "Throw is a single-line spell and must parse to one ability chain"
    );
    parsed.abilities.remove(0)
}

// ===========================================================================
// RESOLVER FIX — Throw from the Saddle, both branches (cast pipeline).
// ===========================================================================

/// Row 1 + Row 3 (the resolver fix). Non-Mount branch: the base creature gets a
/// temporary +1/+1 (no counter), then deals its LIVE power to the foe ONLY; the
/// base creature is the source and takes no damage. Power liveness is proven by
/// varying the base power (4 -> foe 5, 6 -> foe 7).
///
/// Revert-failing assertion: remove the resolver `else if` branch -> the
/// `SequentialSibling` damage tail is dropped in the not-swap branch, so
/// `foe_damage == 0` (and `own_damage == 0`). Both assertions flip.
#[test]
fn throw_non_mount_deals_live_power_to_foe_only() {
    for (base, expected) in [(4i32, 5i32), (6, 7)] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Throw from the Saddle", false, THROW)
            .id();
        // Non-Mount base creature (no "Mount" subtype) -> swap does NOT fire.
        let own = scenario.add_creature(P0, "Plains Rider", base, base).id();
        let foe = scenario.add_creature(P1, "Opposing Bear", 9, 9).id();
        let mut runner = scenario.build();

        let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

        let foe_damage = outcome.state().objects[&foe].damage_marked;
        let own_damage = outcome.state().objects[&own].damage_marked;
        assert_eq!(
            foe_damage, expected as u32,
            "non-Mount base {base}: the base creature must deal its LIVE power \
             ({base} + 1 temp pump = {expected}) to the foe; got {foe_damage}. \
             A value of 0 means the resolver tail-runner (the else-if branch) was \
             reverted and the damage tail was dropped in the not-swap branch."
        );
        assert_eq!(
            own_damage, 0,
            "non-Mount: the base creature is the SOURCE, not a recipient — it must \
             take 0; got {own_damage}."
        );
        assert_eq!(
            outcome.state().objects[&own]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "non-Mount: the +1/+1 COUNTER (the Mount-only override) must NOT be applied"
        );
        assert_eq!(
            outcome.state().objects[&own].power,
            Some(expected),
            "non-Mount: the temporary +1/+1 pump must be applied ({base} -> {expected})"
        );
    }
}

/// Row 2 — Mount branch regression (the SWAP path, unchanged by the fix). The
/// override fires: a permanent +1/+1 COUNTER (not a temp pump) lands on the base
/// creature, which then deals its power (base + counter) to the foe only.
///
/// This is the not-swap fix's negative control: reverting the resolver `else if`
/// does NOT change this case (the swap path at mod.rs:~6844 already delivers the
/// tail), proving the fix is not-swap-only.
#[test]
fn throw_mount_swap_path_applies_counter_and_deals_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Throw from the Saddle", false, THROW)
        .id();
    let own = scenario
        .add_creature(P0, "Mounted Knight", 4, 4)
        .with_subtypes(vec!["Mount"])
        .id();
    let foe = scenario.add_creature(P1, "Opposing Bear", 9, 9).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

    assert_eq!(
        outcome.state().objects[&own]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Mount: the override puts exactly one +1/+1 counter on the base creature"
    );
    assert_eq!(
        outcome.state().objects[&foe].damage_marked,
        5,
        "Mount: the base creature deals its power (4 base + 1 counter = 5) to the foe"
    );
    assert_eq!(
        outcome.state().objects[&own].damage_marked,
        0,
        "Mount: the base creature is the source and takes 0"
    );
}

// ===========================================================================
// RESOLVER FIX — Evil's Thrall, non-Villain branch (cast pipeline).
// HASTE asserted via the full pipeline (continuous-static layer recompute).
// ===========================================================================

/// Row 4 (the resolver fix) — Evil's Thrall, non-Villain branch, full pipeline.
/// P0 controls no Villain, so the swap does NOT fire (gain control until end of
/// turn). The trailing `SequentialSibling` tail — "Untap that creature. It gains
/// haste until end of turn." — must run in this not-swap branch.
///
/// DISCRIMINATING (resolver fix): the gained creature ends UNTAPPED, and a haste
/// `AddKeyword` continuous effect is created (proving the WHOLE tail chain —
/// untap THEN the chained haste sub — executes). Revert the resolver `else if`
/// branch -> the tail is dropped: the creature stays tapped and no haste effect
/// is created.
///
/// PRE-EXISTING LIMITATION (out of scope, documented in the report and the plan's
/// class-completeness boundary): Evil's Thrall's haste does NOT reach the creature
/// due to a pre-existing SelfRef mis-binding in the shared "it gains <keyword>"
/// path (same bug as Reptilian Recruiter, present in both branches); not
/// introduced by this fix; tracked as a follow-up. Concretely, "It gains haste"
/// parses to `GenericEffect{AddKeyword Haste, target: SelfRef}`, so the haste
/// continuous effect is `affected = <the spell>`, NOT the gained creature —
/// measured identical in the swap and not-swap branches. The proper fix is a
/// FIX-A'-class SelfRef→creature rebind that would also alter the swap branch and
/// Reptilian Recruiter, outside this change's 4-card blast radius. The resolver
/// fix's job is only to RUN the tail, which it does. We therefore assert the
/// haste EFFECT is created (tail ran) rather than `victim.has_keyword(Haste)`
/// (which the pre-existing mis-binding makes false in both branches).
#[test]
fn evil_thrall_non_villain_untaps_and_runs_haste_tail() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Evil's Thrall", false, EVIL_THRALL)
        .id();
    // Opponent's creature, tapped — the gain-control target.
    let victim = scenario.add_creature(P1, "Captured Brute", 3, 3).id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&victim).unwrap().tapped = true;

    let outcome = runner.cast(spell).target_object(victim).resolve();

    let obj = &outcome.state().objects[&victim];
    assert_eq!(
        obj.controller, P0,
        "non-Villain: P0 gains control of the target until end of turn"
    );
    assert!(
        !obj.tapped,
        "non-Villain: the trailing 'Untap that creature' tail must fire — the gained \
         creature must be UNTAPPED. Still tapped means the resolver tail-runner was reverted."
    );
    // The chained haste sub of the Untap tail must also execute (CR 608.2c — the
    // whole tail runs). Its continuous effect is created even though the
    // pre-existing SelfRef binding aims it at the spell rather than the creature.
    let haste_effect_created = outcome
        .state()
        .transient_continuous_effects
        .iter()
        .any(|ce| {
            ce.source_id == spell
                && ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        engine::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste
                        }
                    )
                })
        });
    assert!(
        haste_effect_created,
        "non-Villain: the chained 'It gains haste' sub of the Untap tail must execute \
         (a Haste AddKeyword continuous effect sourced from the spell must exist). \
         Absent means the resolver tail-runner stopped after — or before — the untap."
    );
}

// ===========================================================================
// RESOLVER FIX (+ FIX A) — Take the Fall / That's Rough Buddy. "Draw a card."
// prints on a SEPARATE LINE; FIX A recovers it from the line-level swallow, and
// synthesis folds it as the override's `Draw[SequentialSibling]` tail. The
// resolver fix then runs that tail in the not-swap branch. These tests
// discriminate on BOTH fixes (revert either -> hand_drawn == 0; measured).
// ===========================================================================

/// Take the Fall, no-outlaw branch. The base -1/-0 applies (not the outlaw-only
/// -4/-0), and the controller draws a card from the folded `Draw` tail.
///
/// Revert-failing assertion (measured both ways): revert the resolver `else if`
/// -> the folded Draw tail is dropped in the not-swap branch (`hand_drawn == 0`);
/// revert FIX A -> the line-level strip swallows the trailing clause so the Draw
/// never parses (`hand_drawn == 0`).
#[test]
fn take_the_fall_no_outlaw_draws_and_applies_minus_one() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Give P0 a card to draw.
    scenario.add_card_to_library_top(P0, "Spare Card");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Take the Fall", true, TAKE_THE_FALL)
        .id();
    let target = scenario.add_creature(P1, "Sturdy Ogre", 5, 5).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.state().objects[&target].power,
        Some(4),
        "no-outlaw: the base -1/-0 applies (5 -> 4), not the outlaw-only -4/-0"
    );
    outcome.assert_hand_drawn(P0, 1);
}

/// That's Rough Buddy, otherwise (no creature left the battlefield) branch. The
/// override "Put two counters instead" does NOT fire, so the base puts exactly
/// ONE +1/+1 counter; the controller draws a card from the folded `Draw` tail.
///
/// Revert-failing assertion (measured both ways): revert the resolver `else if`
/// -> the folded Draw tail is dropped (`hand_drawn == 0`); revert FIX A -> the
/// Draw is swallowed before synthesis (`hand_drawn == 0`).
#[test]
fn thats_rough_buddy_otherwise_one_counter_and_draws() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Spare Card");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "That's Rough Buddy", true, THATS_ROUGH_BUDDY)
        .id();
    let target = scenario.add_creature(P0, "Eager Recruit", 1, 1).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.state().objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "otherwise: exactly ONE +1/+1 counter (the two-counter override must NOT fire)"
    );
    outcome.assert_hand_drawn(P0, 1);
}

// ===========================================================================
// UNCHANGED FIXTURES — these `ConditionInstead` cards must NOT change behavior.
// ===========================================================================

const FROM_FATHER_TO_SON: &str = "Search your library for a Vehicle card, reveal it, \
and put it into your hand. If this spell was cast from a graveyard, put that card onto \
the battlefield instead. Then shuffle.";

/// Row 7 — From Father to Son, cast from hand (not-swap, NOT cast from a
/// graveyard). This card's `ConditionInstead` override carries a DISTINCT
/// `else_ability` (the put-into-hand continuation), so the not-swap arm takes the
/// EXISTING `if let Some(base_chain) = sub.else_ability` path and the new tail-
/// runner `else if` is never reached — no double-run. The Vehicle ends in HAND
/// (not the battlefield), proving the unchanged else path still works.
///
/// NOTE (class-completeness boundary, documented honestly): From Father to Son IS
/// a member of the "X instead if Z. Then Y." class, but its trailing `Shuffle`
/// sibling is deliberately NOT run by the new branch in the not-swap path
/// (else_ability is present, so the new branch is skipped). Its from-hand shuffle
/// is supplied by `SearchLibrary`'s auto-shuffle, not the dropped trailing
/// `Shuffle` node. It is the only such else-present-AND-distinct-sibling card in
/// the corpus; the design intentionally leaves that node out of scope.
#[test]
fn from_father_to_son_from_hand_takes_else_path_no_double_run() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "From Father to Son", false, FROM_FATHER_TO_SON)
        .id();
    let vehicle = scenario.add_card_to_library_top(P0, "Sky Skiff");
    // Give the library card the Vehicle subtype so the search can find it.
    let mut runner = scenario.build();
    {
        let v = runner.state_mut().objects.get_mut(&vehicle).unwrap();
        v.card_types.core_types.push(CoreType::Artifact);
        v.card_types.subtypes.push("Vehicle".to_string());
        v.base_card_types = v.card_types.clone();
    }

    let outcome = runner.cast(spell).search_first_legal().resolve();

    assert_eq!(
        outcome.zone_of(vehicle),
        Zone::Hand,
        "not-swap (cast from hand): the Vehicle goes to HAND via the unchanged \
         else_ability path, NOT onto the battlefield (the graveyard-only override)"
    );
}

const INCREASING_VENGEANCE: &str = "Copy target instant or sorcery spell you control. \
If this spell was cast from a graveyard, copy that spell twice instead. You may choose \
new targets for the copies.";

/// Row 8 — Increasing Vengeance is byte-unchanged. Its copy clause is
/// `Unimplemented` (the engine does not yet model spell-copy here), so the
/// resolver fix never reaches a runnable `ConditionInstead` tail for it. Parsing
/// it must not panic and must still leave the copy clause `Unimplemented` (the
/// `!Unimplemented` resolver guard, exercised directly in the in-module
/// `condition_instead_not_swap_tail_runner_honors_gates` test, keeps any
/// `Unimplemented` tail inert).
#[test]
fn increasing_vengeance_copy_clause_stays_unimplemented() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        INCREASING_VENGEANCE,
        "Increasing Vengeance",
        &[],
        &[],
        &[],
    );
    let has_unimplemented = parsed.abilities.iter().any(|a| {
        fn walk(def: &AbilityDefinition) -> bool {
            matches!(&*def.effect, Effect::Unimplemented { .. })
                || def.sub_ability.as_deref().is_some_and(walk)
                || def.else_ability.as_deref().is_some_and(walk)
        }
        walk(a)
    });
    assert!(
        has_unimplemented,
        "Increasing Vengeance's copy clause must remain Unimplemented (no speculative \
         semantics introduced); the resolver fix must not fabricate a runnable tail for it"
    );
}
