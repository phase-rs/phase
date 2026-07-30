//! Issue #4835: Intimidation Tactics must exile a chosen artifact or creature
//! from the revealed opponent's hand.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const INTIMIDATION_TACTICS: &str =
    "Target opponent reveals their hand. You choose an artifact or creature card from it. Exile that card.";

const INTIMIDATION_TACTICS_FULL: &str = "\
Target opponent reveals their hand. You choose an artifact or creature card from it. Exile that card.\n\
Cycling {3} ({3}, Discard this card: Draw a card.)";

fn reveal_hand_filter(def: &engine::types::ability::AbilityDefinition) -> Option<&TargetFilter> {
    match def.effect.as_ref() {
        Effect::RevealHand { card_filter, .. } => Some(card_filter),
        _ => def.sub_ability.as_deref().and_then(reveal_hand_filter),
    }
}

fn hand_size(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

#[test]
fn intimidation_tactics_parses_exile_continuation() {
    let def = parse_effect_chain(INTIMIDATION_TACTICS, AbilityKind::Spell);
    let sub = def
        .sub_ability
        .as_ref()
        .expect("exile sub after RevealHand");
    assert!(
        matches!(
            sub.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "expected ParentTarget hand exile sub, got {:?}",
        sub.effect
    );
}

#[test]
fn intimidation_tactics_parses_artifact_or_creature_reveal_filter() {
    let def = parse_effect_chain(INTIMIDATION_TACTICS, AbilityKind::Spell);
    let filter = reveal_hand_filter(&def).expect("RevealHand in chain");
    assert!(
        matches!(
            filter,
            TargetFilter::Or { filters }
                if filters.len() == 2
                    && filters.iter().any(|f| matches!(
                        f,
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Artifact)
                    ))
                    && filters.iter().any(|f| matches!(
                        f,
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature)
                    ))
        ),
        "expected artifact-or-creature reveal filter, got {filter:?}"
    );
}

#[test]
fn intimidation_tactics_exiles_chosen_artifact_or_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        )],
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Intimidation Tactics", false, INTIMIDATION_TACTICS_FULL)
        .id();

    let creature_id = scenario.add_creature_to_hand(P1, "Opp Creature", 2, 2).id();
    let artifact = scenario.add_card_to_hand(P1, "Opp Artifact");
    scenario.add_card_to_hand(P1, "Opp Instant");

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&artifact).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
    }
    let p1_hand_before = hand_size(&runner, P1);

    runner.cast(spell).resolve();

    let eligible = match &runner.state().waiting_for {
        WaitingFor::RevealChoice { cards, .. } => cards.clone(),
        other => panic!("expected RevealChoice after reveal, got {other:?}"),
    };
    assert_eq!(eligible.len(), 2, "artifact and creature must be choosable");
    assert!(eligible.contains(&creature_id));
    assert!(eligible.contains(&artifact));

    let pick = creature_id;
    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("choose creature to exile");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&pick].zone,
        Zone::Exile,
        "chosen card must be exiled"
    );
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before - 1,
        "opponent hand must shrink by the exiled card"
    );
}
