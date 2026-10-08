//! Runtime regression for GH issue #4759 "bug 1" — Chandra's Ignition ("Target
//! creature you control deals damage equal to its power to each other creature
//! and each opponent") reportedly prompted for a SECOND target when the
//! controller had multiple creatures on the battlefield.
//!
//! CR 601.2c: "each"-language ("each other creature", "each opponent") does not
//! target — it is a filtered/blanket recipient set evaluated at resolution.
//! Chandra's Ignition has exactly ONE real target: the damage-source creature
//! ("target creature you control"). Everything else is a population scan.
//!
//! This test drives the REAL cast/target-selection pipeline (not just the
//! parser) with a board shaped exactly like the report: the controller has
//! TWO creatures (the chosen source + one other), plus an opponent creature.
//! It asserts:
//!   1. Exactly ONE `WaitingFor::TargetSelection` prompt occurs before the
//!      spell can be cast (the source-creature picker) — no second prompt for
//!      "each other creature" / "each opponent".
//!   2. The chosen creature is the damage SOURCE (its power is the amount, and
//!      damage-source-sensitive replacements read the chosen creature, not the
//!      spell) — CR 120.1 / CR 608.2c, the DamageAll `damage_source: Target`
//!      binding already added for the Nova Flame class of card (#4960/#5834).
//!   3. Every OTHER creature (both the controller's own second creature and
//!      the opponent's creature) takes damage equal to the source's power, and
//!      the opponent player also loses that much life — but the source itself
//!      takes none (CR 601.2c "each other creature" excludes the source).
//!
//! Verified against current `origin/main` (2026-07-15): the AST already lowers
//! to `TargetOnly { source } -> DamageAll { damage_source: Some(Target),
//! player_filter: Some(Opponent), .. }` (see the existing
//! `target_subject_damage_compound_each_creature_and_each_opponent` parser
//! test), and the generalized "one-sided" quantity-slot suppression added for
//! GH #4234 (Bite Down) + carried into the `damage_source: Target` shape for
//! #4960 (Nova Flame) already covers `Effect::DamageAll` explicitly
//! (`one_sided_fight_source_supplies_quantity_creature` in
//! `game/ability_utils.rs`) — so this reproduces as ALREADY FIXED, not a live
//! bug. This test pins that so a future regression is caught.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHANDRAS_IGNITION: &str = "Target creature you control deals damage equal to its power to \
     each other creature and each opponent.";

#[test]
fn chandras_ignition_prompts_for_exactly_one_target_with_multiple_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // The creature we'll target as the damage source.
    let source = scenario.add_vanilla(P0, 4, 4);
    // A SECOND creature the controller also controls — the exact "multiple
    // creatures" condition from the bug report. Must NOT be prompted for.
    let other_own = scenario.add_vanilla(P0, 1, 6);
    // An opponent's creature — part of the "each other creature" blanket set.
    let opp_creature = scenario.add_vanilla(P1, 1, 6);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chandra's Ignition", false, CHANDRAS_IGNITION)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting Chandra's Ignition (zero cost) must be accepted");

    // Drive every TargetSelection prompt to completion, counting how many
    // distinct slots are offered before the spell is fully cast, then let the
    // stack resolve. Bug report: a SECOND prompt (for the "each other
    // creature"/"each opponent" recipients) should NOT appear.
    let mut prompt_count = 0;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                prompt_count += 1;
                assert!(
                    prompt_count <= 1,
                    "Chandra's Ignition must surface exactly ONE target prompt (the source \
                     creature you control); a second prompt appeared, reproducing GH #4759 \
                     bug 1 (\"each other creature\"/\"each opponent\" incorrectly solicited as \
                     targets, CR 601.2c violation)"
                );
                let legal = &selection.current_legal_targets;
                let obj = |id: ObjectId| engine::types::ability::TargetRef::Object(id);
                assert!(
                    legal.contains(&obj(source)),
                    "the only prompt must offer the controller's own creatures as the damage \
                     source, legal = {legal:?}"
                );
                assert!(
                    !legal.contains(&obj(opp_creature)),
                    "the source-creature prompt must not offer an opponent's creature \
                     (\"target creature YOU CONTROL\"), legal = {legal:?}"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(obj(source)),
                    })
                    .expect("declaring the source creature must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected waiting state while casting Chandra's Ignition: {other:?}"),
        }
    }
    assert_eq!(
        prompt_count, 1,
        "reach-guard: exactly one target prompt (the source creature) must have occurred"
    );

    let state = runner.state();

    // CR 120.1 + CR 208.1: the source's power (4) is dealt to every OTHER
    // creature and each opponent; the source itself takes none.
    assert_eq!(
        state.objects[&source].damage_marked, 0,
        "the chosen source creature must NOT damage itself (\"each OTHER creature\")"
    );
    assert_eq!(
        state.objects[&other_own].damage_marked, 4,
        "the controller's other creature must take damage equal to the source's power (4)"
    );
    assert_eq!(
        state.objects[&opp_creature].damage_marked, 4,
        "the opponent's creature must take damage equal to the source's power (4)"
    );

    let p1_life = state.players.iter().find(|p| p.id == P1).unwrap().life;
    assert_eq!(
        p1_life,
        20 - 4,
        "\"each opponent\" must take damage equal to the source's power (4), via player_filter \
         (a non-targeted recipient set), not a solicited player target"
    );

    // Sanity: nothing died from 4 damage on 6-toughness bodies; zones unchanged.
    assert_eq!(state.objects[&other_own].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opp_creature].zone, Zone::Battlefield);
    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
}
