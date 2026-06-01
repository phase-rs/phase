use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr, QuantityRef, TargetFilter, TypeFilter};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ELEMENTAL_SPECTACLE: &str = "Vivid — Create a number of 5/5 red and green Elemental \
creature tokens equal to the number of colors among permanents you control. Then you gain \
life equal to the number of creatures you control.";

fn create_colored_permanent(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    types: Vec<CoreType>,
    colors: Vec<ManaColor>,
) {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = types;
    obj.color = colors;
}

#[test]
fn elemental_spectacle_counts_distinct_controlled_permanent_colors_for_tokens() {
    let parsed = parse_oracle_text(
        ELEMENTAL_SPECTACLE,
        "Elemental Spectacle",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let definition = parsed
        .abilities
        .first()
        .expect("Elemental Spectacle should parse as a spell ability");

    match definition.effect.as_ref() {
        Effect::Token { count, .. } => match count {
            QuantityExpr::Ref {
                qty: QuantityRef::DistinctColorsAmongPermanents { filter },
            } => match filter {
                TargetFilter::Typed(typed) => {
                    assert_eq!(typed.type_filters, vec![TypeFilter::Permanent]);
                    assert_eq!(
                        typed.controller,
                        Some(engine::types::ability::ControllerRef::You)
                    );
                }
                other => panic!("expected typed permanent filter, got {other:?}"),
            },
            other => panic!("expected distinct-color token count, got {other:?}"),
        },
        other => panic!("expected token effect, got {other:?}"),
    }

    let gain_life = definition
        .sub_ability
        .as_deref()
        .expect("Elemental Spectacle should chain gain life");
    assert!(matches!(
        gain_life.effect.as_ref(),
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. },
            },
            player: TargetFilter::Controller,
        }
    ));

    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(100),
        PlayerId(0),
        "Elemental Spectacle".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];

    create_colored_permanent(
        &mut state,
        101,
        PlayerId(0),
        "Red-Green Creature",
        vec![CoreType::Creature],
        vec![ManaColor::Red, ManaColor::Green],
    );
    create_colored_permanent(
        &mut state,
        102,
        PlayerId(0),
        "Blue Artifact",
        vec![CoreType::Artifact],
        vec![ManaColor::Blue],
    );
    create_colored_permanent(
        &mut state,
        103,
        PlayerId(0),
        "Colorless Land",
        vec![CoreType::Land],
        Vec::new(),
    );
    create_colored_permanent(
        &mut state,
        104,
        PlayerId(1),
        "Opponent Black Creature",
        vec![CoreType::Creature],
        vec![ManaColor::Black],
    );

    let starting_life = state.players[0].life;
    let ability = build_resolved_from_def(definition, source_id, PlayerId(0));
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let created_tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token && obj.controller == PlayerId(0) && obj.name == "Elemental")
        .collect();
    assert_eq!(created_tokens.len(), 3);
    assert!(created_tokens.iter().all(|obj| {
        obj.power == Some(5)
            && obj.toughness == Some(5)
            && obj.color == vec![ManaColor::Red, ManaColor::Green]
            && obj.card_types.core_types.contains(&CoreType::Creature)
            && obj
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Elemental")
    }));
    assert_eq!(state.players[0].life, starting_life + 4);
}
