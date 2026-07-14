//! Regression: GitHub issue #4825 — Feather, the Redeemed.
//!
//! Oracle:
//!   "Flying
//!    Whenever you cast an instant or sorcery spell that targets a creature you
//!    control, exile that card instead of putting it into your graveyard as it
//!    resolves. If you do, return it to your hand at the beginning of the next
//!    end step."
//!
//! Defect: targeted instants/sorceries went to the graveyard instead of being
//! exiled and returned at the next end step. Clause 1 lowered to a no-op
//! `ChangeZone { origin: Graveyard }` (the spell is on the Stack when the
//! trigger resolves, so the CR 400.7 origin guard skipped the move) and the
//! spell then took the CR 608.2n graveyard default.
//!
//! Fix (class: "exile the resolving spell instead of putting it into a
//! graveyard as it resolves", optionally with "return it at the next end
//! step"):
//!   - the parser gate for `Effect::ExileResolvingSpellInsteadOfGraveyard`
//!     accepts the "your graveyard" determiner (CR 614.1a);
//!   - the "If you do, return it …" rider folds onto the carrier as the typed
//!     `then_return: Some(ExiledSpellReturn { destination: Hand, timing:
//!     AtNextPhase { End } })` rider (CR 603.7a + CR 608.2c);
//!   - the return delayed trigger is armed only when the replacement is
//!     actually APPLIED (the spell lands in exile during its own stack
//!     resolution), per CR 603.7a — so a spell countered in response never
//!     arms a return.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, DelayedTriggerCondition, Effect};
use engine::types::actions::GameAction;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FEATHER_TEXT: &str = "Flying\nWhenever you cast an instant or sorcery spell that targets a \
     creature you control, exile that card instead of putting it into your graveyard as it \
     resolves. If you do, return it to your hand at the beginning of the next end step.";

/// Feather's trigger body without the second-sentence return rider — the
/// riderless form of the same class (Rod-shaped, "your graveyard" determiner).
const FEATHER_RIDERLESS_TEXT: &str = "Whenever you cast an instant or sorcery spell that targets \
     a creature you control, exile that card instead of putting it into your graveyard as it \
     resolves.";

/// Rod of Absorption's exile clause (regression guard for the pre-existing
/// riderless class — see issue_2359_rod_of_absorption.rs for its runtime tests).
const ROD_TEXT: &str = "Whenever a player casts an instant or sorcery spell, exile it instead of \
     putting it into a graveyard as it resolves.\n{X}, {T}, Sacrifice this artifact: You may cast \
     any number of spells from among cards exiled with this artifact with total mana value X or \
     less without paying their mana costs.";

/// Walk a def tree (sub/else chains AND `CreateDelayedTrigger` inner bodies)
/// checking whether any node's effect matches the predicate.
fn tree_any(def: &AbilityDefinition, pred: &dyn Fn(&Effect) -> bool) -> bool {
    if pred(&def.effect) {
        return true;
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = &*def.effect {
        if tree_any(effect, pred) {
            return true;
        }
    }
    def.sub_ability
        .as_deref()
        .is_some_and(|s| tree_any(s, pred))
        || def
            .else_ability
            .as_deref()
            .is_some_and(|e| tree_any(e, pred))
}

/// SHAPE: the full Feather text lowers to ONE cast trigger whose entire body is
/// the parameterized carrier — the return rider is the typed `then_return`
/// payload, not an eagerly created `CreateDelayedTrigger` (CR 603.7a: the
/// delayed trigger must be created when the replacement is APPLIED, not when
/// Feather's trigger resolves).
#[test]
fn feather_parses_return_rider_onto_exile_flag() {
    let parsed = parse_oracle_text(
        FEATHER_TEXT,
        "Feather, the Redeemed",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &["Bird".to_string(), "Soldier".to_string()],
    );

    assert_eq!(
        parsed.triggers.len(),
        1,
        "expected exactly one cast trigger, got {:?}",
        parsed.triggers
    );
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the cast trigger must have an execute body");

    // Positive reach-guard: the body parsed to the exact carrier with the
    // rider folded in (this also proves the parse did not degrade to
    // Effect::Unimplemented, so the negative assertions below are non-vacuous).
    match &*execute.effect {
        Effect::ExileResolvingSpellInsteadOfGraveyard { then_return } => {
            let rider = then_return
                .as_ref()
                .expect("the 'If you do, return it …' rider must fold onto the carrier");
            assert_eq!(
                rider.destination,
                Zone::Hand,
                "the rider returns the exiled card to its owner's hand"
            );
            assert_eq!(
                rider.timing,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                "the rider fires at the beginning of the next end step"
            );
        }
        other => panic!("expected ExileResolvingSpellInsteadOfGraveyard, got {other:?}"),
    }
    assert!(
        execute.sub_ability.is_none(),
        "the folded rider must leave no residual sub-ability: {:?}",
        execute.sub_ability
    );
    assert!(
        !tree_any(execute, &|e| matches!(
            e,
            Effect::CreateDelayedTrigger { .. }
        )),
        "no eager CreateDelayedTrigger may remain anywhere in the chain (CR 603.7a)"
    );
    assert!(
        !tree_any(execute, &|e| matches!(e, Effect::Unimplemented { .. })),
        "the folded chain must contain no Unimplemented node"
    );
}

/// SHAPE: the riderless clause (no second sentence) keeps the rider absent.
#[test]
fn feather_riderless_clause_parses_with_flag_off() {
    let parsed = parse_oracle_text(
        FEATHER_RIDERLESS_TEXT,
        "Riderless Feather",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "triggers: {:?}", parsed.triggers);
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the cast trigger must have an execute body");
    match &*execute.effect {
        Effect::ExileResolvingSpellInsteadOfGraveyard { then_return } => assert!(
            then_return.is_none(),
            "without the return sentence the rider must stay absent"
        ),
        other => panic!("expected ExileResolvingSpellInsteadOfGraveyard, got {other:?}"),
    }
    assert!(execute.sub_ability.is_none());
}

/// SHAPE regression: Rod of Absorption's clause (no return rider) still lowers
/// with no rider — the Feather parameterization must not disturb the
/// pre-existing riderless class.
#[test]
fn rod_of_absorption_clause_keeps_flag_off() {
    let parsed = parse_oracle_text(
        ROD_TEXT,
        "Rod of Absorption",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    let execute = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_deref())
        .expect("Rod must have a cast trigger with an execute body");
    match &*execute.effect {
        Effect::ExileResolvingSpellInsteadOfGraveyard { then_return } => assert!(
            then_return.is_none(),
            "Rod has no return rider; the rider must stay absent"
        ),
        other => panic!("expected ExileResolvingSpellInsteadOfGraveyard, got {other:?}"),
    }
}

/// SHAPE (serde compat): the unit→struct parameterization must keep old
/// card-data records deserializing (`{"type":"…"}` with no field → rider
/// `None`) and keep the riderless form's serialization byte-identical (the
/// rider is `skip_serializing_if` when `None`), so Rod of Absorption's
/// exported record does not change. The Feather form must round-trip its
/// typed rider.
#[test]
fn exile_resolving_effect_serde_stays_compatible() {
    let old_record = r#"{"type":"ExileResolvingSpellInsteadOfGraveyard"}"#;
    let effect: Effect = serde_json::from_str(old_record).expect("old unit-variant record");
    assert!(matches!(
        effect,
        Effect::ExileResolvingSpellInsteadOfGraveyard { then_return: None }
    ));
    let reserialized = serde_json::to_string(&effect).expect("serialize");
    assert_eq!(
        reserialized, old_record,
        "riderless serialization must stay byte-identical to the pre-change record"
    );

    // Round-trip the Feather form: the typed rider must survive serde intact.
    let feather = Effect::ExileResolvingSpellInsteadOfGraveyard {
        then_return: Some(engine::types::ability::ExiledSpellReturn {
            destination: Zone::Hand,
            timing: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        }),
    };
    let json = serde_json::to_string(&feather).expect("serialize Feather form");
    let back: Effect = serde_json::from_str(&json).expect("round-trip Feather form");
    assert_eq!(back, feather, "the typed rider must round-trip losslessly");
}

/// Positive runtime path: a targeted instant resolves, is exiled instead of
/// going to the graveyard (CR 614.1a), and returns to its owner's hand at the
/// beginning of the next end step (CR 603.7a), with the one-shot delayed
/// trigger consumed (CR 603.7b).
#[test]
fn feather_exiles_targeted_spell_and_returns_it_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _feather = scenario
        .add_creature(P0, "Feather, the Redeemed", 3, 4)
        .from_oracle_text(FEATHER_TEXT)
        .id();
    let soldier = scenario.add_creature(P0, "Loyal Soldier", 1, 1).id();
    let pump = scenario
        .add_spell_to_hand(P0, "Combat Trick", true)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text("Target creature gets +1/+0 until end of turn.")
        .id();

    let mut runner = scenario.build();

    // Cast the instant targeting P0's own creature; Feather's trigger resolves
    // above it, stamping the exile-instead + return riders; the spell then
    // resolves and is exiled instead of hitting the graveyard.
    let outcome = runner.cast(pump).target_object(soldier).resolve();
    // CR 614.1a: exiled instead of the CR 608.2n graveyard default.
    outcome.assert_zone(&[pump], Zone::Exile);

    // CR 603.7a: the return delayed trigger was armed when the replacement was
    // applied (the spell landed in exile).
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "arming must create exactly one return delayed trigger"
    );

    // CR 603.7a: "at the beginning of the next end step" — cross combat by
    // declaring no attackers (a turn-based action plain priority passes cannot
    // answer), advance to the end step, and drain the fired trigger.
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&pump].zone,
        Zone::Hand,
        "the exiled spell must return to its owner's hand at the next end step (CR 603.7a)"
    );
    // CR 603.7b: one-shot — the delayed trigger is gone after firing.
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot return trigger must be consumed"
    );
}

/// Counter-negative: if the targeted spell is countered after Feather's
/// trigger resolves, the spell goes to the graveyard (CR 701.6a) and NO return
/// is armed — per CR 603.7a the return delayed trigger only exists once the
/// exile-instead replacement is actually applied, which never happens for a
/// countered spell.
#[test]
fn countered_spell_goes_to_graveyard_and_never_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _feather = scenario
        .add_creature(P0, "Feather, the Redeemed", 3, 4)
        .from_oracle_text(FEATHER_TEXT)
        .id();
    let soldier = scenario.add_creature(P0, "Loyal Soldier", 1, 1).id();
    let pump = scenario
        .add_spell_to_hand(P0, "Combat Trick", true)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text("Target creature gets +1/+0 until end of turn.")
        .id();
    let counter = scenario
        .add_spell_to_hand(P1, "Cancel It", true)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text("Counter target spell.")
        .id();

    let mut runner = scenario.build();

    // Commit the pump spell to the stack (Feather's trigger goes on top of it).
    let _ = runner.cast(pump).target_object(soldier).commit();

    // CR 603.3b + CR 608.2: both players pass once — the TOP entry (Feather's
    // trigger) resolves, stamping the riders on the still-on-stack spell.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");

    // Positive reach-guard: the trigger really stamped the return marker on
    // the spell — the negative assertions below are about the COUNTER path,
    // not about the marker never being set.
    assert!(
        runner.state().objects[&pump]
            .exile_from_stack_return_rider
            .is_some(),
        "Feather's resolved trigger must stamp the return rider on the spell"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the pump spell remains on the stack after the trigger resolves"
    );

    // Give P1 priority, then counter the pump spell.
    for _ in 0..4 {
        if runner.state().priority_player == P1 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass to P1");
    }
    let outcome = runner.cast(counter).target_object(pump).resolve();

    // CR 701.6a: the countered spell is put into its owner's graveyard — the
    // replacement never applied, so no exile and no return.
    outcome.assert_zone(&[pump], Zone::Graveyard);
    // CR 400.7: the stack exit cleared the transient rider (zones.rs) — this
    // makes the zone-exit clear non-vacuous, not just the absence of arming.
    assert!(
        runner.state().objects[&pump]
            .exile_from_stack_return_rider
            .is_none(),
        "the zone-exit cleanup must clear the return rider on the countered spell"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "a countered spell must never arm the return delayed trigger (CR 603.7a)"
    );

    // Advance to end step (crossing combat with no attackers) and drain — the
    // spell must stay in the graveyard.
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&pump].zone,
        Zone::Graveyard,
        "no return may fire for a countered spell"
    );
}

/// Fizzle-negative: if the targeted spell's SOLE target becomes illegal after
/// Feather's trigger resolves (the creature is destroyed in response), the
/// spell doesn't resolve (CR 608.2b) and is put into its owner's graveyard —
/// NOT exiled — and no return is armed: per CR 603.7a the return delayed
/// trigger only exists once the exile-instead replacement is actually applied,
/// and a fizzled spell never reaches the exile-instead move. This exercises
/// the fizzle early-return path in `stack.rs`, distinct from the counter path.
#[test]
fn fizzled_spell_goes_to_graveyard_and_never_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _feather = scenario
        .add_creature(P0, "Feather, the Redeemed", 3, 4)
        .from_oracle_text(FEATHER_TEXT)
        .id();
    let soldier = scenario.add_creature(P0, "Loyal Soldier", 1, 1).id();
    let pump = scenario
        .add_spell_to_hand(P0, "Combat Trick", true)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text("Target creature gets +1/+0 until end of turn.")
        .id();
    let removal = scenario
        .add_spell_to_hand(P1, "Sudden Demise", true)
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text("Destroy target creature.")
        .id();

    let mut runner = scenario.build();

    // Commit the pump spell to the stack (Feather's trigger goes on top of it).
    let _ = runner.cast(pump).target_object(soldier).commit();

    // CR 603.3b + CR 608.2: both players pass once — the TOP entry (Feather's
    // trigger) resolves, stamping the riders on the still-on-stack spell.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");

    // Positive reach-guard: the trigger really stamped the return rider on the
    // spell — the negative assertions below are about the FIZZLE path, not
    // about the rider never being set.
    assert!(
        runner.state().objects[&pump]
            .exile_from_stack_return_rider
            .is_some(),
        "Feather's resolved trigger must stamp the return rider on the spell"
    );

    // Give P1 priority, then destroy the pump spell's sole target in response.
    for _ in 0..4 {
        if runner.state().priority_player == P1 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass to P1");
    }
    let _ = runner.cast(removal).target_object(soldier).resolve();
    assert_eq!(
        runner.state().objects[&soldier].zone,
        Zone::Graveyard,
        "the pump spell's sole target must be gone before it tries to resolve"
    );

    // CR 608.2b: all targets illegal — the spell doesn't resolve; it's removed
    // from the stack and put into its owner's graveyard.
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&pump].zone,
        Zone::Graveyard,
        "a fizzled spell takes the CR 608.2b graveyard put, never the exile replacement"
    );
    // CR 400.7: the stack exit cleared the transient rider (zones.rs).
    assert!(
        runner.state().objects[&pump]
            .exile_from_stack_return_rider
            .is_none(),
        "the zone-exit cleanup must clear the return rider on the fizzled spell"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "a fizzled spell must never arm the return delayed trigger (CR 603.7a)"
    );

    // Advance to end step (crossing combat with no attackers) and drain — the
    // spell must stay in the graveyard.
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&pump].zone,
        Zone::Graveyard,
        "no return may fire for a fizzled spell"
    );
}
