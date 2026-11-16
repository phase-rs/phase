//! Desecration Demon (relato de campo, 27/10/2026: "não trigga durante cada etapa de
//! combate"). O gatilho disparava, mas a IA que ACEITAVA o sacrifício nunca tinha
//! criatura legal: "any opponent may sacrifice a creature of their choice" carimbava
//! `controller: Opponent` no filtro do sacrifício, e o resolvedor avalia o filtro em
//! relação a QUEM ACEITA (`FilterContext::from_source_with_controller(.., promptee)`).
//! "Opponent" virava o dono do Demon, o oponente não tinha alvo legal, o aceite era
//! tratado como impossível e o "If a player does, tap ~ and put a +1/+1 counter" não rodava.
//!
//! Oracle text:
//!   Flying
//!   At the beginning of each combat, any opponent may sacrifice a creature of their
//!   choice. If a player does, tap Desecration Demon and put a +1/+1 counter on it.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DEMON_ORACLE: &str = "Flying\nAt the beginning of each combat, any opponent may sacrifice \
a creature of their choice. If a player does, tap this creature and put a +1/+1 counter on it.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn add_creature(state: &mut GameState, card_id: u64, player: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    id
}

fn demon_trigger(state: &mut GameState) {
    let parsed = parse_oracle_text(
        DEMON_ORACLE,
        "Desecration Demon",
        &[],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_deref())
        .expect("Desecration Demon has a beginning-of-combat trigger");
    let demon = state
        .objects
        .values()
        .find(|o| o.name == "Desecration Demon")
        .expect("demon on battlefield")
        .id;
    let ability = build_resolved_from_def(execute, demon, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &ability, &mut events, 0).unwrap();
}

#[test]
fn opponent_accepts_and_sacrifices_own_creature_demon_taps_and_grows() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let demon = add_creature(&mut state, 10, P0, "Desecration Demon");
    let bears = add_creature(&mut state, 20, P1, "Grizzly Bears");
    let lions = add_creature(&mut state, 21, P1, "Savannah Lions");

    demon_trigger(&mut state);

    match &state.waiting_for {
        WaitingFor::OpponentMayChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected OpponentMayChoice for P1, got {other:?}"),
    }
    apply(
        &mut state,
        P1,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("opponent accept must be handled");

    match &state.waiting_for {
        WaitingFor::MultiTargetSelection {
            player,
            legal_targets,
            ..
        } => {
            assert_eq!(*player, P1, "the accepting opponent picks the sacrifice");
            assert!(
                legal_targets.contains(&bears) && legal_targets.contains(&lions),
                "the opponent's own creatures must be legal, got {legal_targets:?}"
            );
            assert!(
                !legal_targets.contains(&demon),
                "the controller's Demon is not the opponent's to sacrifice"
            );
        }
        other => panic!("expected MultiTargetSelection after accepting, got {other:?}"),
    }
    apply(
        &mut state,
        P1,
        GameAction::SelectCards { cards: vec![bears] },
    )
    .expect("sacrificing the opponent's own creature must be legal");

    // A escolha empilha o sacrifício (e o "If a player does"): ambos passam a prioridade
    // até a pilha resolver.
    for _ in 0..8 {
        let WaitingFor::Priority { player } = state.waiting_for else {
            break;
        };
        if state.stack.is_empty() {
            break;
        }
        apply(&mut state, player, GameAction::PassPriority).expect("pass priority");
    }

    assert_eq!(state.objects[&bears].zone, Zone::Graveyard);
    assert_eq!(state.objects[&lions].zone, Zone::Battlefield);
    let demon_obj = &state.objects[&demon];
    assert!(demon_obj.tapped, "If a player does: tap the Demon");
    assert_eq!(
        demon_obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "If a player does: put a +1/+1 counter on the Demon"
    );
}

#[test]
fn opponent_declines_nothing_happens() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let demon = add_creature(&mut state, 10, P0, "Desecration Demon");
    let bears = add_creature(&mut state, 20, P1, "Grizzly Bears");

    demon_trigger(&mut state);
    apply(
        &mut state,
        P1,
        GameAction::DecideOptionalEffect { accept: false },
    )
    .expect("opponent decline must be handled");

    assert_eq!(state.objects[&bears].zone, Zone::Battlefield);
    let demon_obj = &state.objects[&demon];
    assert!(!demon_obj.tapped);
    assert_eq!(
        demon_obj.counters.get(&CounterType::Plus1Plus1).copied(),
        None
    );
}
