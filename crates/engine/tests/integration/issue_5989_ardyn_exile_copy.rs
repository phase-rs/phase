//! Regression for GitHub issue #5989 — Ardyn, the Usurper's combat-trigger
//! ability that exiles a graveyard creature and creates a copy of it as a 5/5
//! black Demon.
//!
//! Oracle (Final Fantasy crossover, verified via Scryfall):
//!   "At the beginning of combat on your turn, you may exile up to one
//!   target creature card from a graveyard. When you exile a card this way,
//!   create a token that's a copy of it, except it's a 5/5 black Demon."
//!
//! Root cause: `strip_if_you_do_conditional` (the reflexive "when you <verb>
//! this way" gate recognizer) only had active-voice combinators for
//! "you discard"/"you sacrifice" this way — no "you exile this way" arm. The
//! entire "when you exile a card this way, create a token…" clause fell
//! through to `Effect::Unimplemented`, so the ability exiled a card and then
//! silently did nothing — matching the reported "I have to make the copy
//! myself" symptom. Fixed by adding `parse_you_exile_this_way_clause`
//! (`parser/oracle_nom/condition.rs`), mirroring the existing discard/
//! sacrifice siblings, and wiring it into `strip_if_you_do_conditional`.
//!
//! This test drives the real activation -> target-selection -> exile ->
//! reflexive-copy pipeline through the actual game-action interface (not a
//! bare `resolve_ability_chain` call with pre-empty targets — this ability
//! is genuinely TARGETED, so target selection must happen at announce time,
//! same lesson as `issue_4948_samwise_gamgee_sacrifice_target_order.rs`).
//! The combat-trigger condition itself ("at the beginning of combat") is not
//! the bug and is replaced with a trivial activation cost so the body can be
//! driven directly via `GameAction::ActivateAbility`.
//!
//! Discriminating scenario: TWO legal graveyard targets so target selection
//! genuinely pauses instead of auto-resolving a lone candidate (the #4948
//! single-legal-target auto-resolve lesson), one in EACH player's graveyard
//! — per the issue's own "only works for my own graveyard" claim, which
//! this test disproves: the parsed target filter carries no controller
//! restriction.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const ARDYN_ORACLE: &str = "{0}: You may exile up to one target creature card from a \
     graveyard. When you exile a card this way, create a token that's a copy of it, \
     except it's a 5/5 black Demon.";

fn ardyn_ability_index(runner: &GameRunner, ardyn: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&ardyn)
        .expect("ardyn on battlefield")
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Ardyn has a costed activated ability")
}

#[test]
fn ardyn_exiles_opponents_graveyard_creature_and_creates_5_5_black_demon_copy() {
    let mut scenario = GameScenario::new();

    let ardyn = scenario
        .add_creature_from_oracle(P0, "Ardyn, the Usurper", 4, 4, ARDYN_ORACLE)
        .id();

    // Two legal targets — one in EACH player's graveyard — so target
    // selection genuinely pauses (a lone candidate auto-resolves inline on
    // this engine, defeating the point of choosing one) and the fix's "any
    // graveyard, not just yours" claim is actually exercised.
    let own_grave_creature = scenario
        .add_creature_to_graveyard(P0, "Own Graveyard Golem", 3, 3)
        .id();
    let opp_grave_creature = scenario
        .add_creature_to_graveyard(P1, "Opponent's Graveyard Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    let ability_index = ardyn_ability_index(&runner, ardyn);
    runner
        .act(GameAction::ActivateAbility {
            source_id: ardyn,
            ability_index,
        })
        .expect("activating Ardyn's ability must succeed");

    // "You may exile up to one target ..." manifests as an OPTIONAL target
    // slot (min 0, max 1), not a separate accept/decline prompt — this is a
    // genuine CR 601.2c target, resolved at announce time.
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a TargetSelection prompt for the optional exile target, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(own_grave_creature)),
        "the controller's own graveyard creature must be a legal target; legal={legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(opp_grave_creature)),
        "issue #5989: the OPPONENT's graveyard creature must also be a legal target — \
         the filter carries no controller restriction; legal={legal:?}"
    );

    // Choose the opponent's creature — the half of the reported symptom
    // ("only works if I do it to my graveyard") this test is pinning.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(opp_grave_creature)],
        })
        .expect("choosing the opponent's graveyard creature must succeed");

    // Let the activated ability resolve off the stack. The TARGET is locked
    // in at announce time (CR 601.2c, already done above), but whether the
    // "you may" instruction is actually PERFORMED is decided separately at
    // resolution time.
    runner.pass_both_players();
    runner.advance_until_stack_empty();

    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(player, P0, "the controller must be asked, not the opponent");
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accepting the optional exile must succeed");
        }
        other => panic!("expected an OptionalEffectChoice prompt for \"you may\", got {other:?}"),
    }

    assert_eq!(
        runner
            .state()
            .objects
            .get(&opp_grave_creature)
            .map(|o| o.zone),
        Some(Zone::Exile),
        "the chosen creature must actually be exiled"
    );

    // The reflexive "when you exile a card this way, create a token copy of
    // it" must have fired automatically — no further player action needed.
    let demon = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.controller == P0)
        })
        .unwrap_or_else(|| {
            panic!(
                "issue #5989: no Demon token was created — the reflexive copy effect \
                 never fired; battlefield={:?}",
                runner.state().battlefield
            )
        });

    let demon_obj = &runner.state().objects[&demon];
    assert_eq!(
        demon_obj.power,
        Some(5),
        "the copy must be a 5/5, got power={:?}",
        demon_obj.power
    );
    assert_eq!(
        demon_obj.toughness,
        Some(5),
        "the copy must be a 5/5, got toughness={:?}",
        demon_obj.toughness
    );
    assert!(
        demon_obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Demon")),
        "the copy must be a Demon, got subtypes={:?}",
        demon_obj.card_types.subtypes
    );
    assert_ne!(
        demon, opp_grave_creature,
        "the token must be a NEW, distinct object — not the original exiled creature itself"
    );
}
