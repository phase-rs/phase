//! Issue #3312 — Shared Trauma join-forces mill resolves.
//!
//! https://github.com/phase-rs/phase/issues/3312

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, PlayerFilter, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const SHARED_TRAUMA_ORACLE: &str = "Join forces — Starting with you, each player may pay any amount of mana. Each player mills X cards, where X is the total amount of mana paid this way.";

fn resolved_from_definition(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: engine::types::player::PlayerId,
) -> ResolvedAbility {
    let mut resolved = ResolvedAbility::new((*def.effect).clone(), vec![], source_id, controller);
    resolved.kind = def.kind;
    resolved.sub_ability = def
        .sub_ability
        .as_ref()
        .map(|sub| Box::new(resolved_from_definition(sub, source_id, controller)));
    resolved.duration = def.duration.clone();
    resolved.condition = def.condition.clone();
    resolved.optional_targeting = def.optional_targeting;
    resolved.optional = def.optional;
    resolved.target_choice_timing = def.target_choice_timing;
    resolved.description = def.description.clone();
    resolved.min_x_value = def.min_x_value;
    resolved.cant_be_copied = def.cant_be_copied;
    resolved.forward_result = def.forward_result;
    resolved.player_scope = def.player_scope.clone();
    resolved.starting_with = def.starting_with.clone();
    resolved.target_selection_mode = def.target_selection_mode;
    resolved.sub_link = def.sub_link;
    resolved
}

fn parsed_shared_trauma_ability(source: ObjectId) -> ResolvedAbility {
    let parsed = parse_oracle_text(
        SHARED_TRAUMA_ORACLE,
        "Shared Trauma",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let def = parsed.abilities.into_iter().next().expect("spell ability");
    resolved_from_definition(&def, source, P0)
}

#[test]
fn shared_trauma_join_forces_prefix_parses_from_full_oracle() {
    let ability = parsed_shared_trauma_ability(ObjectId(1));
    assert!(
        matches!(ability.effect, Effect::PayCost { .. }),
        "expected join-forces PayCost root, got {:?}",
        ability.effect
    );
    assert_eq!(ability.player_scope, Some(PlayerFilter::All));
    assert!(
        !ability.optional,
        "join-forces payment preamble must not be optional at the spell level"
    );
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("Shared Trauma must chain mill after join-forces payment");
    assert!(
        matches!(sub.effect, Effect::Mill { .. }),
        "join-forces tail must mill each player, got {:?}",
        sub.effect
    );
}

#[test]
fn shared_trauma_mills_both_players_after_join_forces_payments() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("Swamp {i}"));
        scenario.add_card_to_library_top(P1, &format!("Island {i}"));
    }

    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "Shared Trauma", false, SHARED_TRAUMA_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    let ability = parsed_shared_trauma_ability(source);

    for _ in 0..3 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            source,
            false,
            vec![],
        ));
    }
    runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        source,
        false,
        vec![],
    ));

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    let payments: &[(engine::types::player::PlayerId, u32)] = &[(P0, 2), (P1, 1)];
    for (expected_player, amount) in payments {
        match &runner.state().waiting_for {
            WaitingFor::PayAmountChoice { player, .. } => {
                assert_eq!(*player, *expected_player);
            }
            other => panic!("expected PayAmountChoice for {expected_player:?}, got {other:?}"),
        }
        runner
            .act(GameAction::SubmitPayAmount { amount: *amount })
            .expect("submit join forces payment");
    }

    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        3,
        "P0 must mill X=3 cards (2+1 paid total)"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].graveyard.len(),
        3,
        "P1 must mill X=3 cards"
    );
}
