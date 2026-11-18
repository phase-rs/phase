use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KILI: &str = "Storied (If you control three or more artifacts, legendaries, and/or Sagas, you have an enduring story for the rest of the game.)\nAs long as you have an enduring story, you may pay {0} rather than pay the equip cost of the first equip ability you activate each turn.\nWhenever another Dwarf or Equipment you control enters, draw a card. This ability triggers only once each turn.";

const VULSHOK_MORNINGSTAR: &str = "Equipped creature gets +2/+2.
Equip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";

fn add_generic_mana(runner: &mut engine::game::scenario::GameRunner, amount: u32) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn activate_equip(
    runner: &mut engine::game::scenario::GameRunner,
    equipment: ObjectId,
) -> engine::types::game_state::ActionResult {
    runner
        .act(GameAction::ActivateAbility {
            source_id: equipment,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|_| {
            panic!(
                "Equip activation should be legal for {equipment:?} with mana {}",
                runner.state().players[0].mana_pool.total()
            )
        })
}

fn advance_to_postcombat(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_phase(Phase::PostCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("an empty attack declaration should advance the turn");
        runner.advance_to_phase(Phase::PostCombatMain);
    }
}

#[test]
fn kili_alternative_equip_cost_is_story_gated_and_first_per_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kili = scenario
        .add_creature_from_oracle(P0, "Kíli the Resourceful", 1, 2, KILI)
        .as_legendary()
        .with_subtypes(vec!["Dwarf", "Scout"])
        .id();
    let first_equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    let second_equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    let third_equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);

    let mut runner = scenario.build();
    advance_to_postcombat(&mut runner);
    assert!(
        runner.state().enduring_story.contains(&P0),
        "Kíli and three Equipment artifacts should establish an enduring story after state checks"
    );

    let result = activate_equip(&mut runner, first_equipment);
    let zero_cost_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert_eq!(costs.len(), 1, "the unaffordable {{2}} branch is filtered");
            costs
                .iter()
                .position(|cost| {
                    matches!(
                        cost,
                        AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)
                    )
                })
                .expect("the active story should offer the {0} Equip branch")
        }
        other => panic!("expected the story-gated alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: zero_cost_branch,
        })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&first_equipment),
        "the first Equipment should attach after paying the alternative cost"
    );

    add_generic_mana(&mut runner, 2);
    let second = activate_equip(&mut runner, second_equipment);
    assert!(
        !matches!(
            second.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "the alternative applies only to the first Equip activation each turn"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&second_equipment),
        "the second Equipment should still equip normally"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the second activation pays its ordinary {{2}} equip cost"
    );

    runner.advance_to_phase(Phase::PreCombatMain);
    if runner.state().active_player != P0 {
        advance_to_postcombat(&mut runner);
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_eq!(
        runner.state().active_player,
        P0,
        "expected Kíli's next turn; phase={:?}, waiting={:?}, turn={}",
        runner.state().phase,
        runner.state().waiting_for,
        runner.state().turn_number
    );
    let next_turn = activate_equip(&mut runner, third_equipment);
    assert!(
        matches!(
            next_turn.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { ref costs, .. }
                if costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)
                ))
        ),
        "the alternative cost becomes available again on the next turn"
    );
}

#[test]
fn kili_uses_the_normal_equip_cost_without_an_enduring_story() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kili = scenario
        .add_creature_from_oracle(P0, "Kíli the Resourceful", 1, 2, KILI)
        .as_legendary()
        .with_subtypes(vec!["Dwarf", "Scout"])
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    advance_to_postcombat(&mut runner);
    assert!(
        !runner.state().enduring_story.contains(&P0),
        "Kíli and the Equipment alone are below the Storied threshold"
    );
    add_generic_mana(&mut runner, 2);
    let result = activate_equip(&mut runner, equipment);
    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "the alternative branch must not be offered without an enduring story"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&equipment),
        "the ordinary equip ability must still attach the Equipment"
    );
    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "paying the normal Equip cost leaves the Equipment on the battlefield"
    );
}

/// Scope negative: the grant is "you may pay {0}" — only the controller of the
/// granting permanent benefits (CR 109.5). An opponent's Kíli with its enduring
/// story established must not offer the {0} branch to the other player.
#[test]
fn opponent_kili_does_not_offer_the_alternative_equip_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Kíli the Resourceful", 1, 2, KILI)
        .as_legendary()
        .with_subtypes(vec!["Dwarf", "Scout"]);
    scenario
        .add_artifact_from_oracle(P1, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"]);
    scenario
        .add_artifact_from_oracle(P1, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"]);
    let bear = scenario.add_vanilla(P0, 2, 2);
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    advance_to_postcombat(&mut runner);
    assert!(
        runner.state().enduring_story.contains(&P1),
        "reach guard: the opponent's Kíli has its condition met"
    );
    add_generic_mana(&mut runner, 2);
    let result = activate_equip(&mut runner, equipment);
    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "an opponent's Kíli must not grant the {{0}} alternative to its opponent"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&bear]
            .attachments
            .contains(&equipment),
        "the Equipment attaches after paying the ordinary {{2}}"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

const FORGE_ANEW: &str = "When this enchantment enters, return target Equipment card from your graveyard to the battlefield.\nDuring your turn, you may activate equip abilities any time you could cast an instant.\nYou may pay {0} rather than pay the equip cost of the first equip ability you activate during each of your turns.";

const LEONIN_SHIKARI: &str = "You may activate equip abilities any time you could cast an instant.";

/// Forge Anew + Leonin Shikari + Vulshok Morningstar for P0. Shikari is P0's only
/// creature, so it is the sole legal equip target. Shikari's grant lets P0 equip at
/// instant speed, so P0 can equip during the opponent's turn
/// (`opponents_turn == true`), which is where Forge Anew's "each of your turns"
/// gate must keep the {0} branch away.
fn forge_anew_scenario(
    opponents_turn: bool,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Forge Anew", FORGE_ANEW);
    let shikari = scenario
        .add_creature_from_oracle(P0, "Leonin Shikari", 1, 1, LEONIN_SHIKARI)
        .with_subtypes(vec!["Cat", "Soldier"])
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    if opponents_turn {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, shikari, equipment)
}

/// CR 118.9 + CR 702.6a: Forge Anew's {0} Equip alternative is limited to the first
/// equip ability activated "during each of your turns". Leonin Shikari lets P0
/// equip during the opponent's turn, where no {0} branch may be offered.
#[test]
fn forge_anew_zero_equip_is_not_offered_during_the_opponents_turn() {
    let (mut runner, shikari, equipment) = forge_anew_scenario(true);
    assert_eq!(runner.state().active_player, P1);
    add_generic_mana(&mut runner, 2);

    let result = activate_equip(&mut runner, equipment);
    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "Forge Anew's {{0}} applies only during your own turn"
    );
    resolve_stack_by_passing(&mut runner);
    assert!(
        runner.state().objects[&shikari]
            .attachments
            .contains(&equipment),
        "reach guard: Shikari made the instant-speed equip legal and it paid the ordinary {{2}}"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// The same fixture on P0's own turn: the {0} branch is offered (and is the only
/// affordable one with an empty pool) and paying it equips the creature.
#[test]
fn forge_anew_zero_equip_is_offered_during_your_own_turn() {
    let (mut runner, shikari, equipment) = forge_anew_scenario(false);
    assert_eq!(runner.state().active_player, P0);

    let result = activate_equip(&mut runner, equipment);
    let zero_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(
                |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)),
            )
            .expect("Forge Anew should offer the {0} Equip branch on your turn"),
        other => panic!("expected the alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch { index: zero_branch })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&shikari]
            .attachments
            .contains(&equipment),
        "the Equipment attaches after paying {{0}}"
    );
}

const BRUENOR: &str = "Each creature you control gets +2/+0 for each Equipment attached to it.\nYou may pay {0} rather than pay the equip cost of the first equip ability you activate each turn.";

/// CR 109.5 + CR 118.9 + CR 702.6a: Bruenor Battlehammer's "the first equip
/// ability you activate each turn" counts only its controller's own activations.
/// P1 equips first (Leonin Shikari, at instant speed, during P0's turn); P0's
/// first equip is still offered {0}.
#[test]
fn opponents_earlier_equip_does_not_consume_bruenors_zero_equip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Bruenor Battlehammer", 5, 3, BRUENOR)
        .as_legendary()
        .with_subtypes(vec!["Dwarf", "Warrior"]);
    let p0_equipment = scenario
        .add_artifact_from_oracle(P0, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    let shikari = scenario
        .add_creature_from_oracle(P1, "Leonin Shikari", 1, 1, LEONIN_SHIKARI)
        .with_subtypes(vec!["Cat", "Soldier"])
        .id();
    let p1_equipment = scenario
        .add_artifact_from_oracle(P1, "Vulshok Morningstar", VULSHOK_MORNINGSTAR)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();

    // CR 117.3d: P0 passes with an empty stack, so P1 receives priority during
    // P0's turn and can equip at instant speed (Leonin Shikari).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 may pass priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach guard: P1 received priority"
    );
    for _ in 0..2 {
        runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let p1_result = runner
        .act(GameAction::ActivateAbility {
            source_id: p1_equipment,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1's instant-speed equip is legal");
    assert!(
        !matches!(
            p1_result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "Bruenor belongs to P0, so P1 is offered no {{0}} branch"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&shikari]
            .attachments
            .contains(&p1_equipment),
        "reach guard: P1's equip activated and resolved first this turn"
    );
    // CR 117.3b: P0, the active player, receives priority after the ability resolves.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "P0 naturally regains priority"
    );

    // P0 has no mana, so only the {0} alternative is payable.
    let result = activate_equip(&mut runner, p0_equipment);
    let zero_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(
                |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)),
            )
            .expect("the {0} equip branch should still be offered to P0"),
        other => panic!("expected the alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch { index: zero_branch })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&p0_equipment].attached_to.is_some(),
        "P0's Equipment attaches after paying {{0}}"
    );
}

/// CR 117.4 + CR 608.1: with an ability on the stack, both players pass in
/// turn until it resolves.
fn resolve_stack_by_passing(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the stack");
    }
    panic!("stack did not resolve: {:?}", runner.state().stack);
}
