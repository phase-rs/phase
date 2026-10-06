//! Issue #8455 — Town Greeter: "When this creature enters, mill four cards.
//! You may put a land card from among them into your hand. If you put a Town
//! card into your hand this way, you gain 2 life."
//!
//! The parse was already correct: the `GainLife` sub carries
//! `AbilityCondition::ZoneChangedThisWay { Subtype("Town"), Hand }`. The defect
//! was at runtime. "You may put a land card from among them" pauses at
//! `WaitingFor::EffectZoneChoice`, and a paused parent effect still stamps
//! `state.last_zone_changed_ids` from its own event slice — empty, because
//! nothing has moved yet. The rider is correctly deferred onto the ability
//! continuation, but the choice's completion handler never republished the
//! ledger, so the drain re-evaluated `ZoneChangedThisWay` against an empty set:
//! false for every card, so the life gain was silently dropped.
//!
//! CR 608.2c ("this way" scopes to the objects the resolving instruction moved)
//! + CR 400.7 (the moved object is the referent).
//!
//! CLASS, NOT CARD: every `ZoneChangedThisWay` rider whose parent zone change is
//! a player selection routes through the same completion handler — Nashi,
//! Spelunking, Oviya, Rulik Mons, The Vast Scrier. `spelunking_class_*` below is
//! that shape with NO preceding zone-change instruction, so it pins the fix at
//! the interactive-selection layer rather than at Town Greeter's mill. The
//! negated twin ("If you didn't put a card … this way") is the same defect at
//! the opposite polarity: an empty ledger makes `Not { ZoneChangedThisWay }`
//! fire unconditionally.
//!
//! DISCRIMINATION: revert the `last_zone_changed_ids` republish in
//! `engine_resolution_choices.rs` and the two positive legs read the empty
//! ledger and gain 0 life, while their non-matching partners keep passing —
//! which is why each asserts a matching AND a non-matching selection against
//! the same body. `patient_naturalist_*` pins the opposite sign, where the
//! empty ledger produces an EXTRA effect rather than a missing one.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityCondition, AbilityKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const TOWN_GREETER: &str = "mill four cards. you may put a land card from among them into your \
hand. if you put a town card into your hand this way, you gain 2 life.";

/// Spelunking's shape without the preceding draw: the selected zone change is
/// the FIRST instruction, so its rider has no earlier ledger to accidentally
/// read. Pre-fix the ledger is empty here too (cleared at chain depth 0).
const SPELUNKING: &str = "you may put a land card from your hand onto the battlefield. if you put \
a cave onto the battlefield this way, you gain 4 life.";

/// Give a seeded generic card the land type line the body filters on.
fn make_land(runner: &mut GameRunner, id: ObjectId, subtypes: &[&str]) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Land];
    obj.card_types.subtypes = subtypes.iter().map(|s| (*s).to_string()).collect();
    obj.base_card_types = obj.card_types.clone();
}

/// Whether a rider's condition gates on `ZoneChangedThisWay` — directly, or
/// under the `Not` wrapper the negated riders carry (Patient Naturalist's "if
/// you can't").
///
/// WIDENED DELIBERATELY, AND CONTROLLED. The original form accepted only the
/// bare variant. That is the same conflation of *negation* with *composition*
/// that mislabelled the card census, encoded a second time in the harness. A
/// widened assertion that quietly stops rejecting would void the precondition
/// for every leg below without turning anything red, so
/// `the_gate_predicate_still_rejects_an_unrelated_condition` pins both signs.
fn gates_on_zone_changed_this_way(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::ZoneChangedThisWay { .. } => true,
        AbilityCondition::Not { condition } => gates_on_zone_changed_this_way(condition),
        _ => false,
    }
}

#[test]
fn the_gate_predicate_still_rejects_an_unrelated_condition() {
    let zctw = || AbilityCondition::ZoneChangedThisWay {
        filter: TargetFilter::Any,
        destination: None,
    };
    // Reach guards: the two shapes the harness must accept really are accepted.
    assert!(gates_on_zone_changed_this_way(&zctw()));
    assert!(gates_on_zone_changed_this_way(&AbilityCondition::Not {
        condition: Box::new(zctw()),
    }));
    // The control: widening must not have turned the gate into a tautology.
    assert!(
        !gates_on_zone_changed_this_way(&AbilityCondition::IsYourTurn),
        "an unrelated condition must still be refused"
    );
    // The one a loose `Not` arm fails: it must inspect its inner condition, not
    // report true merely because a `Not` is present.
    assert!(
        !gates_on_zone_changed_this_way(&AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        }),
        "Not-wrapping an unrelated condition must not launder it through the gate"
    );
}

/// Resolve `body` as a top-level chain and assert it really is the gated shape
/// under test — a body that stopped parsing the rider would make every
/// assertion below vacuous.
fn resolve_body(runner: &mut GameRunner, body: &str, source: ObjectId) {
    let def = parse_effect_chain(body, AbilityKind::Spell);
    let gate = def
        .sub_ability
        .as_ref()
        .and_then(|sub| sub.sub_ability.as_ref())
        .or(def.sub_ability.as_ref())
        .and_then(|sub| sub.condition.as_ref());
    assert!(
        gate.is_some_and(gates_on_zone_changed_this_way),
        "{body:?} must lower its rider to ZoneChangedThisWay — got {gate:?}"
    );
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).expect("chain resolves");
}

/// Accept the "you may …" yes/no that precedes the card pick. Town Greeter's
/// "you may put a land card from among them" lowers the optional onto the
/// `ChangeZone` itself (`up_to`), so it goes straight to the selection; a body
/// whose optional is its own decision (Spelunking's "you may put a land card
/// from your hand") pauses here first.
fn accept_optional(runner: &mut GameRunner) {
    assert_eq!(
        runner.waiting_for_kind(),
        "OptionalEffectChoice",
        "this body's optional is expected to be its own decision"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional is legal");
}

/// Assert the chain is parked on the card pick — the interactive path the fix
/// lives on. Without this a body that resolved straight through would make
/// every assertion below vacuous.
fn expect_selection(runner: &GameRunner) {
    assert_eq!(
        runner.waiting_for_kind(),
        "EffectZoneChoice",
        "the put must pause for a card selection"
    );
}

/// Answer the pending `EffectZoneChoice` and report the controller's life delta.
fn select(runner: &mut GameRunner, cards: Vec<ObjectId>) -> i32 {
    let before = runner.state().players[0].life;
    runner
        .act(GameAction::SelectCards { cards })
        .expect("selection is legal");
    runner.state().players[0].life - before
}

/// Town Greeter with four cards milled and both a Town and a non-Town land
/// among them, so the same board answers both polarities.
fn town_greeter_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Town Greeter", 1, 1).id();
    let town = scenario.add_card_to_library_top(P0, "Adventurer's Inn");
    let plains = scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Filler A");
    scenario.add_card_to_library_top(P0, "Filler B");
    let mut runner = scenario.build();
    make_land(&mut runner, town, &["Town"]);
    make_land(&mut runner, plains, &["Plains"]);
    (runner, source, town, plains)
}

#[test]
fn town_greeter_gains_two_life_when_a_town_is_put_into_hand_this_way() {
    let (mut runner, source, town, _) = town_greeter_board();
    resolve_body(&mut runner, TOWN_GREETER, source);
    expect_selection(&runner);
    assert_eq!(
        select(&mut runner, vec![town]),
        2,
        "Town put into hand → +2"
    );
    assert_eq!(
        runner.state().objects[&town].zone,
        Zone::Hand,
        "the Town really was put into hand — the life gain is what was missing"
    );
}

#[test]
fn town_greeter_gains_no_life_when_the_land_put_into_hand_is_not_a_town() {
    let (mut runner, source, _, plains) = town_greeter_board();
    resolve_body(&mut runner, TOWN_GREETER, source);
    expect_selection(&runner);
    assert_eq!(
        select(&mut runner, vec![plains]),
        0,
        "a non-Town land put this way gains nothing"
    );
    assert_eq!(runner.state().objects[&plains].zone, Zone::Hand);
}

#[test]
fn town_greeter_gains_no_life_when_the_optional_put_is_declined() {
    let (mut runner, source, town, _) = town_greeter_board();
    resolve_body(&mut runner, TOWN_GREETER, source);
    expect_selection(&runner);
    assert_eq!(
        select(&mut runner, vec![]),
        0,
        "declining the put gains nothing even with a Town among the milled cards"
    );
    assert_eq!(
        runner.state().objects[&town].zone,
        Zone::Graveyard,
        "the declined Town stayed in the graveyard the mill put it in"
    );
}

/// The same rider on a selected zone change with NO preceding zone-change
/// instruction: the ledger the rider reads can only have been published by the
/// selection itself.
fn spelunking_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Spelunking", 1, 1).id();
    let cave = scenario
        .add_land_to_hand(P0, "Sunken Citadel")
        .with_subtypes(vec!["Cave"])
        .id();
    let plains = scenario
        .add_land_to_hand(P0, "Plains")
        .with_subtypes(vec!["Plains"])
        .id();
    (scenario.build(), source, cave, plains)
}

#[test]
fn spelunking_class_gains_life_when_the_land_put_onto_the_battlefield_is_a_cave() {
    let (mut runner, source, cave, _) = spelunking_board();
    resolve_body(&mut runner, SPELUNKING, source);
    accept_optional(&mut runner);
    expect_selection(&runner);
    assert_eq!(select(&mut runner, vec![cave]), 4, "Cave put this way → +4");
    assert_eq!(runner.state().objects[&cave].zone, Zone::Battlefield);
}

#[test]
fn spelunking_class_gains_no_life_when_the_land_put_onto_the_battlefield_is_not_a_cave() {
    let (mut runner, source, _, plains) = spelunking_board();
    resolve_body(&mut runner, SPELUNKING, source);
    accept_optional(&mut runner);
    expect_selection(&runner);
    assert_eq!(
        select(&mut runner, vec![plains]),
        0,
        "a non-Cave land put this way gains nothing"
    );
    assert_eq!(runner.state().objects[&plains].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Patient Naturalist — the NEGATED twin of the same ledger omission.
// ---------------------------------------------------------------------------
//
// "When this creature enters, mill three cards. Put a land card from among the
// milled cards into your hand. If you can't, create a Treasure token."
//
//   Mill{3} → ChangeZone{Hand, TrackedSetFiltered(Land), up_to}
//           → Token{Treasure} gated on Not { ZoneChangedThisWay { Any } }
//
// This is the opposite sign of the Town Greeter bug and the one that hides. An
// always-empty ledger makes `Not { ZoneChangedThisWay }` true unconditionally,
// so pre-fix Patient Naturalist mints a Treasure EVEN WHEN a land was put into
// hand — an extra permanent, rather than a missing life gain.
//
// PRE-FIX REDNESS IS DEDUCED, NOT RE-RUN. The probe on unmodified origin/main
// measured `last_zone_changed_ids = []` after the selection completed, with the
// chosen card confirmed in hand. `Not { ZoneChangedThisWay { Any } }` over an
// empty ledger is true by construction, so the Treasure is minted. No revert
// build was run for this leg.
const PATIENT_NATURALIST: &str = "mill three cards. put a land card from among the milled cards \
into your hand. if you can't, create a treasure token.";

fn treasures(runner: &GameRunner) -> usize {
    runner
        .battlefield_names()
        .iter()
        .filter(|name| name.as_str() == "Treasure")
        .count()
}

#[test]
fn patient_naturalist_creates_no_treasure_when_a_land_is_put_into_hand() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Patient Naturalist", 1, 1).id();
    let forest = scenario.add_card_to_library_top(P0, "Forest");
    scenario.add_card_to_library_top(P0, "Filler A");
    scenario.add_card_to_library_top(P0, "Filler B");
    let mut runner = scenario.build();
    make_land(&mut runner, forest, &["Forest"]);

    resolve_body(&mut runner, PATIENT_NATURALIST, source);
    expect_selection(&runner);
    select(&mut runner, vec![forest]);

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "the land really was put into hand — that is what makes the gate false"
    );
    assert_eq!(
        treasures(&runner),
        0,
        "a land WAS put this way, so `if you can't` must NOT fire"
    );
}

/// Non-vacuity partner for the leg above: it proves the Treasure branch can fire
/// at all, so the zero there is not an artifact of an inert `Token` effect.
///
/// SCOPE: this leg does NOT exercise the republish under test. With no land
/// among the milled cards the eligible set is empty, so the `ChangeZone` raises
/// no `EffectZoneChoice` and the rider is evaluated on the inline path. The
/// `assert_ne!` below is what keeps that claim checkable rather than asserted.
#[test]
fn patient_naturalist_creates_a_treasure_when_no_land_was_milled() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Patient Naturalist", 1, 1).id();
    scenario.add_card_to_library_top(P0, "Filler A");
    scenario.add_card_to_library_top(P0, "Filler B");
    scenario.add_card_to_library_top(P0, "Filler C");
    let mut runner = scenario.build();

    resolve_body(&mut runner, PATIENT_NATURALIST, source);
    assert_ne!(
        runner.waiting_for_kind(),
        "EffectZoneChoice",
        "an empty eligible set must raise no selection — this leg is the inline path"
    );
    assert_eq!(
        treasures(&runner),
        1,
        "no land was milled, so `if you can't` fires"
    );
}
