//! Issue #2423 — Deadly Brew: sacrificing a planeswalker must open the optional
//! return-from-graveyard rider.
//!
//! Oracle:
//!   "Each player sacrifices a creature or planeswalker of their choice. If you
//!    sacrificed a permanent this way, you may return another permanent card
//!    from your graveyard to your hand."

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DEADLY_BREW_ORACLE: &str = "Each player sacrifices a creature or planeswalker of their choice. If you sacrificed a permanent this way, you may return another permanent card from your graveyard to your hand.";

fn deadly_brew(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(DEADLY_BREW_ORACLE, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn add_battlefield_permanent(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
    oid
}

fn resolve_through_sacrifice_choices(state: &mut GameState) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10, "stuck waiting: {:?}", state.waiting_for);
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, player, .. } => {
                let pick = cards
                    .iter()
                    .find(|id| {
                        state.objects.get(id).is_some_and(|obj| {
                            obj.card_types.core_types.contains(&CoreType::Planeswalker)
                        })
                    })
                    .or_else(|| cards.first())
                    .copied()
                    .expect("eligible sacrifice");
                engine::game::engine::apply(
                    state,
                    *player,
                    GameAction::SelectCards { cards: vec![pick] },
                )
                .expect("sacrifice choice");
            }
            _ => break,
        }
    }
}

#[test]
fn deadly_brew_planeswalker_sacrifice_opens_optional_return() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Deadly Brew".to_string(),
        Zone::Stack,
    );

    let pw = add_battlefield_permanent(&mut state, 10, PlayerId(0), "Jace", CoreType::Planeswalker);
    let _p1_creature =
        add_battlefield_permanent(&mut state, 11, PlayerId(1), "Bear", CoreType::Creature);
    let returnable = create_object(
        &mut state,
        CardId(20),
        PlayerId(0),
        "Returned Permanent".to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&returnable).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();

    let ability = deadly_brew(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    resolve_through_sacrifice_choices(&mut state);

    assert!(
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "controller sacrificed a planeswalker — optional return must prompt, got {:?}",
        state.waiting_for
    );
    assert!(
        state
            .objects
            .get(&pw)
            .is_some_and(|o| o.zone == Zone::Graveyard),
        "planeswalker should be in graveyard after sacrifice"
    );
}
