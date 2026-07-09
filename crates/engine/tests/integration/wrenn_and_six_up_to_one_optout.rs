//! Issue #2389: Wrenn and Six's +1 — "Return up to one target land card from
//! your graveyard to your hand."
//!
//! "Up to one target" allows declining (CR 115.6: a spell or ability that
//! requires targets may allow zero to be chosen; CR 601.2c: the variable target
//! count is announced at activation). When the controller chooses ZERO targets,
//! the ability must resolve doing nothing — with no lingering selection prompt.
//!
//! The +1 parses to a graveyard-to-hand `Effect::ChangeZone` with
//! `multi_target { min: 0, max: 1 }`. Declining leaves `ability.targets` empty,
//! and the resolver must short-circuit that empty targeted up-to-one return to
//! "do nothing" with no follow-up zone choice.
//!
//! These tests drive the real activation pipeline via `GameRunner::activate`.

use std::sync::Arc;

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::CounterType;

const WRENN_PLUS_ONE: &str = "Return up to one target land card from your graveyard to your hand.";

fn wrenn_plus_one_definition() -> AbilityDefinition {
    let mut def = parse_effect_chain(WRENN_PLUS_ONE, AbilityKind::Activated);
    def.cost = Some(AbilityCost::Loyalty { amount: 1 });
    def
}

/// Build a battlefield Wrenn-and-Six planeswalker with the parsed +1 loyalty
/// ability, and seed P0's graveyard with a single land card. Returns the
/// runner, the planeswalker id, and the land id.
fn setup() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wrenn_id = scenario.add_creature(P0, "Wrenn and Six", 0, 0).id();
    let mut runner = scenario.build();

    // `create_object(..., Zone::Graveyard)` adds the card to P0's graveyard.
    let land_id = create_object(
        runner.state_mut(),
        CardId(900),
        P0,
        "Mountain".to_string(),
        Zone::Graveyard,
    );
    {
        let land = runner.state_mut().objects.get_mut(&land_id).unwrap();
        land.card_types.core_types = vec![CoreType::Land];
        land.base_card_types = land.card_types.clone();
    }

    {
        let obj = runner.state_mut().objects.get_mut(&wrenn_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.loyalty = Some(3);
        obj.counters.insert(CounterType::Loyalty, 3);

        let plus_one = wrenn_plus_one_definition();
        Arc::make_mut(&mut obj.abilities).push(plus_one.clone());
        Arc::make_mut(&mut obj.base_abilities).push(plus_one);
    }

    (runner, wrenn_id, land_id)
}

fn graveyard_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .graveyard
        .len()
}

/// The +1 parses to a targeted "up to one" graveyard-to-hand return:
/// `multi_target { min: 0 }` makes `targeting_is_optional()` true. This is the
/// structural fact the runtime fix relies on, and the parser must keep emitting it.
#[test]
fn plus_one_parses_as_optional_targeted_graveyard_return() {
    let def = parse_effect_chain(WRENN_PLUS_ONE, AbilityKind::Activated);
    assert!(
        matches!(
            &*def.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                ..
            }
        ),
        "expected graveyard-to-hand ChangeZone, got {:?}",
        def.effect
    );
    let spec = def
        .multi_target
        .as_ref()
        .expect("up-to-one must carry a multi_target spec");
    assert!(
        spec.min_is_fixed_zero(),
        "up-to-one must allow zero targets (min == 0)"
    );
}

/// Declining the up-to-one target resolves the ability cleanly: no card is
/// returned and no selection prompt is left pending. Fails pre-fix (the resolver
/// surfaced an `EffectZoneChoice` for the graveyard land instead).
#[test]
fn declining_the_target_resolves_with_no_prompt_and_no_effect() {
    let (mut runner, wrenn, _land) = setup();

    // `AbilityActivation` declares no target — the driver submits `None` for the
    // optional slot, exercising the zero-target opt-out.
    runner.activate(wrenn, 0).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "declining the up-to-one target must resolve to a Priority window with no \
         lingering selection prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        graveyard_len(runner.state(), P0),
        1,
        "no land is returned when zero targets are chosen"
    );
}

/// The select-one path still works: choosing the land returns it to hand.
#[test]
fn selecting_the_land_returns_it_to_hand() {
    let (mut runner, wrenn, land) = setup();

    runner.activate(wrenn, 0).target_object(land).resolve();

    assert_eq!(
        runner.state().objects.get(&land).map(|o| o.zone),
        Some(Zone::Hand),
        "the chosen land card returns to its owner's hand"
    );
    assert_eq!(
        graveyard_len(runner.state(), P0),
        0,
        "the returned land leaves the graveyard"
    );
}
