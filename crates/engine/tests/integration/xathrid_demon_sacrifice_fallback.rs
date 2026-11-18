//! Xathrid Demon — the "can't sacrifice" fallback reads the SACRIFICE'S result,
//! not the opponent life-loss clause that follows it.
//!
//! Oracle (Scryfall `cards/named?exact=Xathrid Demon`): "At the beginning of your
//! upkeep, sacrifice a creature other than Xathrid Demon, then each opponent
//! loses life equal to the sacrificed creature's power. If you can't sacrifice a
//! creature, tap Xathrid Demon and you lose 7 life."
//!
//! CR anchors:
//!   - CR 608.2c: instructions are followed in the order written; "can't
//!     sacrifice" refers to the sacrifice instruction, not to the life-loss
//!     clause between them (which moves nothing).
//!   - CR 608.2f: "each opponent loses life" is one action over several players.
//!   - CR 701.21a: to sacrifice a permanent, its controller moves it to the
//!     graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::PlayerFilter;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (keywords included).
const XATHRID_DEMON: &str = "Flying, trample\n\
     At the beginning of your upkeep, sacrifice a creature other than Xathrid Demon, \
     then each opponent loses life equal to the sacrificed creature's power. If you \
     can't sacrifice a creature, tap Xathrid Demon and you lose 7 life.";

const P2: PlayerId = PlayerId(2);
const FODDER_POWER: i32 = 3;
const START_LIFE: i32 = 20;

/// Three players; P1 controls the Demon (and fodder when `with_fodder`). The turn
/// passes from P0's end step into P1's upkeep, which puts the Demon's trigger on
/// the stack. Returns the runner after that trigger resolved, the Demon, and the
/// fodder creature if any.
fn p1_upkeep(with_fodder: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    p1_upkeep_with(false, with_fodder)
}

/// `include_controller` widens the life-loss clause's scope from "each opponent"
/// to "each player" after parsing, so the controller's own seat leg runs too.
fn p1_upkeep_with(
    include_controller: bool,
    with_fodder: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 1707);
    scenario.at_phase(Phase::End);
    let demon = scenario
        .add_creature_from_oracle(P1, "Xathrid Demon", 7, 7, XATHRID_DEMON)
        .id();
    let fodder = with_fodder.then(|| {
        scenario
            .add_creature(P1, "Fodder Bear", FODDER_POWER, 3)
            .id()
    });
    for seat in 0..3 {
        // CR 504.1: P1's draw step must find a card.
        scenario.add_spell_to_library_top(PlayerId(seat), "Library Filler", false);
        scenario.add_spell_to_library_top(PlayerId(seat), "Library Filler", false);
    }
    let mut runner = scenario.build();
    if include_controller {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&demon)
            .expect("demon exists");
        for entry in std::sync::Arc::make_mut(&mut object.trigger_definitions.0) {
            let clause = entry
                .definition
                .execute
                .as_mut()
                .and_then(|execute| execute.sub_ability.as_mut())
                .expect("the life-loss clause follows the sacrifice");
            clause.player_scope = Some(PlayerFilter::All);
        }
    }
    for _ in 0..16 {
        if runner.state().phase == Phase::Upkeep {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("advance toward P1's upkeep");
    }
    assert_eq!(runner.state().phase, Phase::Upkeep, "reach guard");
    assert_eq!(runner.state().active_player, P1, "reach guard");
    assert!(
        !runner.state().stack.is_empty(),
        "reach guard: the upkeep trigger must be on the stack"
    );
    runner.advance_until_stack_empty();
    (runner, demon, fodder)
}

#[test]
fn sacrificing_a_creature_skips_the_fallback_and_drains_each_opponent() {
    let (runner, demon, fodder) = p1_upkeep(true);
    let fodder = fodder.expect("fodder staged");
    let start = START_LIFE;
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the other creature is sacrificed"
    );
    assert_eq!(
        runner.state().objects[&demon].zone,
        Zone::Battlefield,
        "the Demon is not the sacrifice"
    );
    assert!(
        !runner.state().objects[&demon].tapped,
        "CR 608.2c: a performed sacrifice means the fallback must not tap the Demon"
    );
    assert_eq!(
        runner.life(P1),
        start,
        "the controller must not lose 7 life after a successful sacrifice"
    );
    for opponent in [P0, P2] {
        assert_eq!(
            runner.life(opponent),
            start - FODDER_POWER,
            "each opponent loses life equal to the sacrificed creature's power"
        );
    }
}

#[test]
fn no_other_creature_taps_the_demon_and_costs_seven() {
    let (runner, demon, _) = p1_upkeep(false);
    assert!(
        runner.state().objects[&demon].tapped,
        "with nothing to sacrifice the Demon taps"
    );
    assert_eq!(
        runner.life(P1),
        START_LIFE - 7,
        "the controller loses 7 life"
    );
    for opponent in [P0, P2] {
        assert_eq!(runner.life(opponent), START_LIFE, "opponents lose nothing");
    }
}

/// No printed card pairs a sacrifice with a non-moving "each player" clause, so
/// this is Xathrid Demon with its life-loss clause widened to every player after
/// parsing (`p1_upkeep_with`). The controller's own seat leg (original controller
/// == acting controller) must leave the sacrifice's ledger alone too (CR 608.2c).
#[test]
fn each_player_clause_including_the_controller_seat_keeps_the_sacrifice_result() {
    let (runner, demon, fodder) = p1_upkeep_with(true, true);
    let fodder = fodder.expect("fodder staged");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert!(
        !runner.state().objects[&demon].tapped,
        "CR 608.2c: the controller's own seat leg must not erase the sacrifice"
    );
    for opponent in [P0, P2] {
        assert_eq!(runner.life(opponent), START_LIFE - FODDER_POWER);
    }
    // The 7-life fallback must not have run for the controller (their own leg's
    // amount is outside this ledger test's scope).
    assert!(
        runner.life(P1) > START_LIFE - 7,
        "no 7 life fallback after a performed sacrifice"
    );
}

// ---------------------------------------------------------------------------
// SYNTHETIC scoped-Seek witnesses (no printed card pairs a per-player Seek with
// a "this way" gate). CR 608.2c: a clause's "this way" result is what the clause
// actually did — a scoped Seek that moves cards publishes them, and a Seek that
// finds nothing has a real empty result that must not leak an earlier clause's
// moves.
// ---------------------------------------------------------------------------

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    TypedFilter,
};
use engine::types::zones::EtbTapState;

const GATE_LIFE: i32 = 3;

/// `Seek a land card from your library into your hand` scoped to every player.
fn scoped_land_seek() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Seek {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            from_top: None,
            destination: Zone::Hand,
            enter_tapped: EtbTapState::Unspecified,
        },
    )
    .player_scope(PlayerFilter::All)
}

/// "If a card changed zones this way, you gain 3 life."
fn this_way_life_gate() -> AbilityDefinition {
    let mut gate = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: GATE_LIFE },
            player: TargetFilter::Controller,
        },
    );
    gate.condition = Some(AbilityCondition::ZoneChangedThisWay {
        filter: TargetFilter::Any,
        destination: None,
    });
    gate
}

fn two_player_runner(libraries_have_lands: bool) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Seek Source", 1, 1).id();
    let mut library_cards = Vec::new();
    for seat in [P0, P1] {
        let id = if libraries_have_lands {
            scenario.add_land_to_library_top(seat, "Library Land").id()
        } else {
            scenario
                .add_spell_to_library_top(seat, "Library Spell", false)
                .id()
        };
        library_cards.push(id);
    }
    (scenario.build(), source, library_cards)
}

/// (a) A scoped Seek that moves a card for each player publishes exactly those
/// moves: the gate sees them. Under the effect-name table (Seek unlisted) the
/// pre-clause ledger was restored, the gate read an empty ledger, and no life
/// was gained.
#[test]
fn synthetic_scoped_seek_that_moves_cards_publishes_them_to_the_gate() {
    let (mut runner, source, library_cards) = two_player_runner(true);
    let def = scoped_land_seek().sub_ability(this_way_life_gate());
    let ability = build_resolved_from_def(&def, source, P0);
    let life_before = runner.life(P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("scoped seek resolves");

    for card in &library_cards {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Hand,
            "each player's seek moved their land to hand"
        );
    }
    assert_eq!(
        runner.life(P0),
        life_before + GATE_LIFE,
        "CR 608.2c: the gate reads the Seek's own moves"
    );
}

/// (b) A prior clause moved a creature; the scoped Seek then finds nothing. Its
/// empty result is a real result: the gate must see an empty ledger, not the
/// sacrificed creature. Under the old name table the Seek counted as nonmoving
/// and the sacrifice's ledger leaked through, so the gate fired.
#[test]
fn synthetic_scoped_seek_finding_nothing_clears_the_prior_clause_ledger() {
    let (mut runner, source, _) = two_player_runner(false);
    let mut head = parse_effect_chain("sacrifice a creature", AbilityKind::Spell);
    head.sub_ability = Some(Box::new(
        scoped_land_seek().sub_ability(this_way_life_gate()),
    ));
    let ability = build_resolved_from_def(&head, source, P0);
    let life_before = runner.life(P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("sacrifice then scoped seek resolve");

    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Graveyard,
        "reach guard: the prior clause moved something"
    );
    assert!(
        runner.state().last_zone_changed_ids.is_empty(),
        "the empty Seek result replaces the sacrifice's ledger"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "CR 608.2c: the gate must not read the prior clause's moves"
    );
}
