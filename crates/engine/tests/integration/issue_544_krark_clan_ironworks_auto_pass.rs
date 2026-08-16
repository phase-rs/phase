//! Issue #544 — Krark-Clan Ironworks auto-pass during priority.
//!
//! Oracle: `Sacrifice an artifact: Add {C}{C}.`
//!
//! Sacrifice-for-mana abilities are exposed via `legal_actions_by_object` but
//! omitted from the flat `legal_actions` list. `auto_pass_recommended` must
//! still consult activatable sacrifice mana sources so the client does not
//! auto-pass when the player may want to sac for triggers or mana.

use engine::ai_support::{
    auto_pass_recommended, has_meaningful_priority_action, legal_actions_full,
};
use engine::game::apply_as_current;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
    SacrificeCost, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use std::sync::Arc;

const P0: PlayerId = PlayerId(0);

fn priority_main_phase(state: &mut GameState, player: PlayerId) {
    state.phase = Phase::PreCombatMain;
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    state.turn_number = 2;
}

fn add_kci(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(1001),
        player,
        "Krark-Clan Ironworks".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.summoning_sick = false;
    Arc::make_mut(&mut obj.abilities).push(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 2 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            1,
        ))),
    );
    id
}

fn add_artifact_creature(
    state: &mut GameState,
    player: PlayerId,
    card_id: u64,
    name: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(1);
    obj.toughness = Some(1);
    obj.summoning_sick = false;
    id
}

// CR 605.3a + CR 603.6 (narrowed): Sacrificing a permanent for mana stays a
// meaningful priority decision for the CR 732.5 loop firewall (the classifier
// `has_meaningful_priority_action` still returns true). But on a BARE board —
// vanilla artifact fodder with no death-trigger payoff, nothing castable, no
// mana-costed activated ability — the sacrifice enables no concrete follow-up,
// so the frontend auto-pass recommendation now fires. Classifier and
// recommendation deliberately DISAGREE for this bare case.
#[test]
fn auto_pass_fires_for_bare_kci_sacrifice_mana() {
    let mut state = GameState::new_two_player(42);
    priority_main_phase(&mut state, P0);

    let kci = add_kci(&mut state, P0);
    let _fodder = add_artifact_creature(&mut state, P0, 2001, "Myr Retriever");

    let (flat, _, grouped) = legal_actions_full(&state);

    let kci_activation = GameAction::ActivateAbility {
        source_id: kci,
        ability_index: 0,
    };
    assert!(
        grouped
            .get(&kci)
            .is_some_and(|actions| actions.contains(&kci_activation)),
        "KCI sacrifice mana must be in legal_actions_by_object during priority"
    );
    assert!(
        !flat.contains(&kci_activation),
        "precondition: sacrifice mana stays out of flat legal_actions during priority"
    );
    // Classifier reach-guard (UNCHANGED): proves we truly reach the sac rung and
    // the loop firewall still counts the sacrifice as meaningful (CR 605.3a +
    // 603.6) — so this test is non-vacuous and the disagreement below is real.
    assert!(
        has_meaningful_priority_action(&state, &flat),
        "classifier still counts sacrifice-for-mana as meaningful (loop firewall intact)"
    );
    // Flipped-on-revert assertion: with a bare board the sacrifice enables no
    // downstream follow-up, so auto-pass now fires.
    assert!(
        auto_pass_recommended(&state, &flat),
        "bare KCI sacrifice-for-mana with no downstream follow-up → auto-pass fires"
    );
}

// CR 605.3b: Activating KCI during priority resolves inline and adds mana.
#[test]
fn kci_activation_during_priority_adds_mana_to_pool() {
    let mut state = GameState::new_two_player(42);
    priority_main_phase(&mut state, P0);

    let kci = add_kci(&mut state, P0);
    let retriever = add_artifact_creature(&mut state, P0, 2001, "Myr Retriever");

    apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: kci,
            ability_index: 0,
        },
    )
    .expect("KCI activation during priority must be legal");

    match &state.waiting_for {
        engine::types::game_state::WaitingFor::PayCost {
            kind: engine::types::game_state::PayCostKind::Sacrifice,
            ..
        } => {}
        other => panic!("Expected sacrifice choice prompt, got {other:?}"),
    }

    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![retriever],
        },
    )
    .expect("Sacrifice retriever for KCI");

    assert_eq!(
        state.players[P0.0 as usize].mana_pool.total(),
        2,
        "KCI must add {{C}}{{C}} to the mana pool"
    );
    assert!(
        state.players[P0.0 as usize].graveyard.contains(&retriever),
        "Sacrificed artifact must be in graveyard"
    );
}
